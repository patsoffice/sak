//! Sole chokepoint for `kube::Client` and `kube::Api` access.
//!
//! Every other module in `src/k8s/` must route Kubernetes API access through
//! the helpers exposed here. Importing `kube::Api` (or any of its mutation
//! methods) anywhere else in the domain is forbidden, and the
//! [`tests::no_mutation_methods_outside_client_module`] grep test enforces it
//! on every `cargo test --features k8s` run.
//!
//! `kube` does not provide a read-only client variant, so this convention is
//! the only thing that keeps the domain provably free of writes.

use anyhow::{Context, Result};
use k8s_openapi::api::core::v1::Pod;
use kube::api::{ApiResource, DynamicObject, ListParams, LogParams, ObjectList};
use kube::{Api, Client};

/// Build a `kube::Client` from the standard sources, in order:
///
/// 1. `KUBECONFIG` env var (or `~/.kube/config`),
/// 2. In-cluster service account if running inside a pod.
///
/// Same sources as [`kube::Client::try_default`], plus kubectl's `NO_PROXY`
/// handling for an environment-supplied proxy (see [`bypass_env_proxy`]).
pub async fn build_client() -> Result<Client> {
    // `Client::try_default()` is infer-then-build; doing the two steps here
    // leaves room to apply NO_PROXY, which kube does not implement.
    let mut config = kube::Config::infer()
        .await
        .context("failed to build kubernetes client (kubeconfig or in-cluster)")?;
    let env_proxy = env_var_either("HTTPS_PROXY", "https_proxy");
    let no_proxy = env_var_either("NO_PROXY", "no_proxy");
    if bypass_env_proxy(
        config.proxy_url.as_ref(),
        env_proxy.as_deref(),
        no_proxy.as_deref(),
        &config.cluster_url,
    ) {
        config.proxy_url = None;
    }
    Client::try_from(config).context("failed to build kubernetes client (kubeconfig or in-cluster)")
}

/// First non-empty value of an upper- or lower-case environment variable.
fn env_var_either(upper: &str, lower: &str) -> Option<String> {
    [upper, lower]
        .iter()
        .filter_map(|k| std::env::var(k).ok())
        .find(|v| !v.trim().is_empty())
}

/// Whether to drop the client's proxy so the cluster is reached directly.
///
/// kube takes its proxy from the kubeconfig's `proxy-url`, else from
/// `HTTPS_PROXY`, but never consults `NO_PROXY`. kubectl does (Go's
/// `http.ProxyFromEnvironment`), so a cluster listed in `NO_PROXY` would be
/// reached directly by kubectl but pushed through the proxy by sak. Only an
/// environment-sourced proxy is dropped: a `proxy-url` written into the
/// kubeconfig is an explicit per-cluster choice and always wins (it is
/// recognised by not being the `HTTPS_PROXY` value). Pure and unit-tested.
fn bypass_env_proxy(
    proxy_url: Option<&http::Uri>,
    env_proxy: Option<&str>,
    no_proxy: Option<&str>,
    cluster_url: &http::Uri,
) -> bool {
    let (Some(proxy), Some(env_proxy), Some(no_proxy)) = (proxy_url, env_proxy, no_proxy) else {
        return false;
    };
    let from_env = env_proxy
        .trim()
        .parse::<http::Uri>()
        .is_ok_and(|env| &env == proxy);
    let Some(host) = cluster_url.host() else {
        return false;
    };
    let port = cluster_url.port_u16().or(match cluster_url.scheme_str() {
        Some("http") => Some(80),
        _ => Some(443),
    });
    from_env && no_proxy_matches(no_proxy, host, port)
}

/// Go's `NO_PROXY` rules (`golang.org/x/net/http/httpproxy`): comma or space
/// separated entries; `*` matches everything; an IP literal or CIDR range
/// matches addresses; a domain matches itself and its subdomains, with or
/// without a leading `.`; an entry with `:port` only matches that port.
/// Matching is case-insensitive.
fn no_proxy_matches(no_proxy: &str, host: &str, port: Option<u16>) -> bool {
    let host = host.trim_matches(['[', ']']).to_ascii_lowercase();
    let host_ip: Option<std::net::IpAddr> = host.parse().ok();
    no_proxy
        .split([',', ' '])
        .map(|e| e.trim().to_ascii_lowercase())
        .filter(|e| !e.is_empty())
        .any(|entry| {
            if entry == "*" {
                return true;
            }
            if let Some((net, bits)) = entry.split_once('/') {
                return match (host_ip, net.parse(), bits.parse::<u32>()) {
                    (Some(ip), Ok(net), Ok(bits)) => cidr_contains(net, bits, ip),
                    _ => false,
                };
            }
            // Split an optional `:port`, leaving bare IPv6 literals intact.
            let (name, entry_port) = match entry.rsplit_once(':') {
                Some((n, p)) if !n.contains(':') || n.starts_with('[') => {
                    (n.trim_matches(['[', ']']), p.parse::<u16>().ok())
                }
                _ => (entry.trim_matches(['[', ']']), None),
            };
            if entry_port.is_some() && entry_port != port {
                return false;
            }
            if let (Some(ip), Ok(entry_ip)) = (host_ip, name.parse::<std::net::IpAddr>()) {
                return ip == entry_ip;
            }
            let domain = name.trim_start_matches('.');
            host == domain || host.ends_with(&format!(".{domain}"))
        })
}

/// Whether `ip` falls in `net/bits` (same address family only).
fn cidr_contains(net: std::net::IpAddr, bits: u32, ip: std::net::IpAddr) -> bool {
    use std::net::IpAddr;
    match (net, ip) {
        (IpAddr::V4(n), IpAddr::V4(i)) if bits <= 32 => {
            let mask = u32::MAX.checked_shl(32 - bits).unwrap_or(0);
            (u32::from(n) & mask) == (u32::from(i) & mask)
        }
        (IpAddr::V6(n), IpAddr::V6(i)) if bits <= 128 => {
            let mask = u128::MAX.checked_shl(128 - bits).unwrap_or(0);
            (u128::from(n) & mask) == (u128::from(i) & mask)
        }
        _ => false,
    }
}

/// List resources of a kind, scoped to a namespace or across all namespaces.
///
/// Pass `namespace = None` to list cluster-wide (`all_with`); pass `Some(ns)`
/// for a namespaced list. Caller is responsible for ensuring the kind is
/// actually namespaced when supplying a namespace.
pub async fn list_dyn(
    client: &Client,
    ar: &ApiResource,
    namespace: Option<&str>,
    lp: &ListParams,
) -> Result<ObjectList<DynamicObject>> {
    let api: Api<DynamicObject> = match namespace {
        Some(ns) => Api::namespaced_with(client.clone(), ns, ar),
        None => Api::all_with(client.clone(), ar),
    };
    api.list(lp)
        .await
        .with_context(|| format!("failed to list {}", ar.kind))
}

/// Get a single resource by name.
///
/// Returns `Ok(None)` if the apiserver responds with 404, so callers can map
/// "not found" to the sak-standard exit code 1 without losing the ability to
/// surface other errors as exit code 2.
pub async fn get_dyn(
    client: &Client,
    ar: &ApiResource,
    namespace: Option<&str>,
    name: &str,
) -> Result<Option<DynamicObject>> {
    let api: Api<DynamicObject> = match namespace {
        Some(ns) => Api::namespaced_with(client.clone(), ns, ar),
        None => Api::all_with(client.clone(), ar),
    };
    match api.get(name).await {
        Ok(obj) => Ok(Some(obj)),
        Err(kube::Error::Api(e)) if e.code == 404 => Ok(None),
        Err(e) => Err(e).with_context(|| format!("failed to get {} {}", ar.kind, name)),
    }
}

/// Fetch container logs from a pod, returning the buffered body as a String.
///
/// `Api::<Pod>::logs` is technically a read, not a mutation, but the chokepoint
/// grep test forbids `kube::Api` mentions outside this module — so this helper
/// is the only place callers may go for `sak k8s logs`. Caller fills in
/// container/tail/since/previous on the supplied [`LogParams`]; this helper is
/// otherwise format-agnostic.
pub async fn pod_logs(
    client: &Client,
    namespace: &str,
    pod: &str,
    lp: &LogParams,
) -> Result<String> {
    let api: Api<Pod> = Api::namespaced(client.clone(), namespace);
    api.logs(pod, lp)
        .await
        .with_context(|| format!("failed to fetch logs for pod {namespace}/{pod}"))
}

/// Ask the apiserver whether the current user may perform an action, via
/// `SelfSubjectAccessReview`. Returns `(allowed, reason)`.
///
/// `create` on this virtual resource is the one legitimate read-only use of
/// `Api::create` in the entire domain: the review is never persisted, it only
/// answers the RBAC question. Keep every `.create(` in this module so the
/// chokepoint grep test continues to prove nothing else writes.
pub async fn can_i(
    client: &Client,
    verb: &str,
    group: Option<&str>,
    resource: &str,
    subresource: Option<&str>,
    namespace: Option<&str>,
    name: Option<&str>,
) -> Result<(bool, Option<String>)> {
    use k8s_openapi::api::authorization::v1::{
        ResourceAttributes, SelfSubjectAccessReview, SelfSubjectAccessReviewSpec,
    };
    let review = SelfSubjectAccessReview {
        spec: SelfSubjectAccessReviewSpec {
            resource_attributes: Some(ResourceAttributes {
                group: group.map(str::to_string),
                resource: Some(resource.to_string()),
                verb: Some(verb.to_string()),
                subresource: subresource.map(str::to_string),
                namespace: namespace.map(str::to_string),
                name: name.map(str::to_string),
                ..Default::default()
            }),
            ..Default::default()
        },
        ..Default::default()
    };
    let api: Api<SelfSubjectAccessReview> = Api::all(client.clone());
    let created = api
        .create(&kube::api::PostParams::default(), &review)
        .await
        .context("failed to create SelfSubjectAccessReview")?;
    let status = created.status.unwrap_or_default();
    Ok((status.allowed, status.reason))
}

/// Issue a `GET` against an arbitrary apiserver path and return the response
/// body as text. Used by `sak k8s schema` to fetch the OpenAPI v3 index and
/// per-group documents.
///
/// `path` may be either a server-relative URL (e.g. `/openapi/v3`) or a path
/// with a query string from the OpenAPI index's `serverRelativeURL` field
/// (e.g. `/openapi/v3/apis/apps/v1?hash=abc...`).
pub async fn request_text(client: &Client, path: &str) -> Result<String> {
    let req = http::Request::builder()
        .method("GET")
        .uri(path)
        .body(Vec::new())
        .with_context(|| format!("building request for {path}"))?;
    client
        .request_text(req)
        .await
        .with_context(|| format!("GET {path}"))
}

#[cfg(test)]
mod tests {
    use super::{bypass_env_proxy, no_proxy_matches};

    /// Tokens that must not appear in any `src/k8s/*.rs` file other than
    /// `client.rs`. The directory walk and comment-skip mechanics live in
    /// [`crate::test_support::assert_no_forbidden_tokens`].
    const FORBIDDEN_TOKENS: &[&str] = &[
        "kube::Api",
        "Api::<",
        "Api::namespaced_with",
        "Api::all_with",
        ".create(",
        ".delete(",
        ".delete_collection(",
        ".patch(",
        ".replace(",
        ".patch_scale(",
        "DeleteParams",
        "PatchParams",
    ];

    #[test]
    fn no_mutation_methods_outside_client_module() {
        crate::test_support::assert_no_forbidden_tokens(
            "k8s",
            FORBIDDEN_TOKENS,
            "kube::Api / mutation methods must be confined to src/k8s/client.rs",
        );
    }

    #[test]
    fn no_proxy_follows_go_rules() {
        let m = |entries: &str, host: &str| no_proxy_matches(entries, host, Some(6443));
        assert!(m("*", "anything.example"));
        // Domains match themselves and subdomains, with or without the dot.
        assert!(m("corp.example", "k8s.corp.example"));
        assert!(m(".corp.example", "k8s.corp.example"));
        assert!(m("corp.example", "corp.example"));
        assert!(!m("corp.example", "notcorp.example"));
        assert!(m("Corp.Example", "K8S.corp.example"));
        // IP literals and CIDR ranges, v4 and v6.
        assert!(m("192.168.1.10", "192.168.1.10"));
        assert!(m("10.0.0.0/8,192.168.0.0/16", "192.168.1.10"));
        assert!(!m("10.0.0.0/8", "192.168.1.10"));
        assert!(m("fd00::/8", "[fd00::1]"));
        assert!(m("::1", "[::1]"));
        // A port in the entry must match; mixed separators are fine.
        assert!(m("localhost, 192.168.1.10:6443", "192.168.1.10"));
        assert!(!m("192.168.1.10:443", "192.168.1.10"));
        assert!(!m("", "192.168.1.10"));
    }

    #[test]
    fn env_proxy_is_bypassed_only_for_no_proxy_hosts() {
        // Regression: kube honours HTTPS_PROXY but not NO_PROXY, so with
        // http-proxy enabled an internal cluster went through the proxy
        // where kubectl connects directly.
        let cluster: http::Uri = "https://192.168.1.10:6443".parse().unwrap();
        let proxy: http::Uri = "http://proxy.corp:3128".parse().unwrap();
        let env = Some("http://proxy.corp:3128");
        assert!(bypass_env_proxy(
            Some(&proxy),
            env,
            Some("192.168.0.0/16"),
            &cluster
        ));
        assert!(!bypass_env_proxy(
            Some(&proxy),
            env,
            Some("10.0.0.0/8"),
            &cluster
        ));
        assert!(!bypass_env_proxy(Some(&proxy), env, None, &cluster));
        // A kubeconfig proxy-url (not the HTTPS_PROXY value) always wins.
        let kubeconfig_proxy: http::Uri = "http://bastion:8080".parse().unwrap();
        assert!(!bypass_env_proxy(
            Some(&kubeconfig_proxy),
            env,
            Some("192.168.0.0/16"),
            &cluster
        ));
        // No proxy configured: nothing to bypass.
        assert!(!bypass_env_proxy(None, env, Some("*"), &cluster));
    }

    #[test]
    fn http_proxy_url_builds_client_stack() {
        use kube::Config;
        use kube::client::ClientBuilder;

        // Pin an explicit root so the builder never consults the host's
        // native roots (absent in sandboxes like `nix build`, where
        // SSL_CERT_FILE points at /no-cert-file.crt).
        let pem = include_bytes!("../cert/testdata/sak-test.pem");
        let ders = crate::cert::extract_ders(pem).expect("test cert parses");
        let cluster_url: http::Uri = "https://192.0.2.1:6443".parse().unwrap();
        let mut config = Config::new(cluster_url);
        config.root_cert = Some(ders);
        config.proxy_url = Some("http://proxy.example:8080".parse().unwrap());
        let result = ClientBuilder::try_from(config);
        assert!(
            result.is_ok(),
            "http proxy URL should build with kube/http-proxy enabled: {:?}",
            result.err()
        );
    }

    /// Live-cluster smoke test for the foundation. Exercises `build_client`,
    /// `discovery::resolve` (fast path via the shortname table), and
    /// `list_dyn` against whichever cluster `KUBECONFIG` points at.
    ///
    /// Marked `#[ignore]` so it stays out of the default `cargo test` run.
    /// To run it manually:
    ///
    /// ```text
    /// KUBECONFIG=../home-ops/kubeconfig \
    ///     cargo test --features k8s -- --ignored --nocapture k8s::client::tests::live_smoke
    /// ```
    #[tokio::test]
    #[ignore = "requires a live cluster; set KUBECONFIG and run with --ignored"]
    async fn live_smoke() {
        use crate::k8s::discovery;
        use kube::api::ListParams;

        let client = super::build_client()
            .await
            .expect("build_client failed — is KUBECONFIG set and the cluster reachable?");

        // Fast-path resolve for `pod` via the hardcoded shortname table.
        let (ar, _caps) = discovery::resolve(&client, "pod")
            .await
            .expect("resolve(pod) failed");
        assert_eq!(ar.kind, "Pod");
        assert_eq!(ar.group, "");
        assert_eq!(ar.version, "v1");

        // List pods cluster-wide via the chokepoint. We don't assert on count
        // because empty clusters are valid; we only assert the call succeeds
        // and that the returned items deserialize.
        let pods = super::list_dyn(&client, &ar, None, &ListParams::default())
            .await
            .expect("list_dyn(pods) failed");
        eprintln!(
            "live_smoke: discovered {} pod(s) cluster-wide",
            pods.items.len()
        );

        // Slow-path resolve: pick a kind that's not in the shortname table.
        // `Lease` from coordination.k8s.io exists on every modern cluster.
        let (lease, _) = discovery::resolve(&client, "Lease")
            .await
            .expect("resolve(Lease) via slow path failed");
        assert_eq!(lease.kind, "Lease");
        eprintln!(
            "live_smoke: slow-path resolved Lease -> {}/{}",
            lease.group, lease.version
        );
    }
}
