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
use kube::api::{ApiResource, DynamicObject, ListParams, ObjectList};
use kube::{Api, Client};

/// Build a `kube::Client` from the standard sources, in order:
///
/// 1. `KUBECONFIG` env var (or `~/.kube/config`),
/// 2. In-cluster service account if running inside a pod.
///
/// Wraps [`kube::Client::try_default`].
#[allow(dead_code)] // wired up by sak-llm-ovb (kinds + get) and others
pub async fn build_client() -> Result<Client> {
    Client::try_default()
        .await
        .context("failed to build kubernetes client (kubeconfig or in-cluster)")
}

/// List resources of a kind, scoped to a namespace or across all namespaces.
///
/// Pass `namespace = None` to list cluster-wide (`all_with`); pass `Some(ns)`
/// for a namespaced list. Caller is responsible for ensuring the kind is
/// actually namespaced when supplying a namespace.
#[allow(dead_code)] // wired up by sak-llm-ovb (kinds + get)
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
#[allow(dead_code)] // wired up by sak-llm-ovb (kinds + get)
pub async fn get_dyn(
    client: &Client,
    ar: &ApiResource,
    namespace: Option<&str>,
    name: &str,
) -> Result<DynamicObject> {
    let api: Api<DynamicObject> = match namespace {
        Some(ns) => Api::namespaced_with(client.clone(), ns, ar),
        None => Api::all_with(client.clone(), ar),
    };
    api.get(name)
        .await
        .with_context(|| format!("failed to get {} {}", ar.kind, name))
}

#[cfg(test)]
mod tests {
    use std::ffi::OsStr;
    use std::fs;
    use std::path::PathBuf;

    /// Tokens that must not appear in any `src/k8s/*.rs` file other than
    /// `client.rs`. Comments are exempt — the skip logic below ignores any
    /// line whose first non-whitespace characters are `//`.
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
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/k8s");
        let entries = fs::read_dir(&dir).expect("read src/k8s");

        let mut violations = Vec::new();
        for entry in entries {
            let entry = entry.expect("dir entry");
            let path = entry.path();
            if path.extension() != Some(OsStr::new("rs")) {
                continue;
            }
            if path.file_name() == Some(OsStr::new("client.rs")) {
                continue;
            }

            let content = fs::read_to_string(&path).expect("read source file");
            for (idx, line) in content.lines().enumerate() {
                let trimmed = line.trim_start();
                // Skip line comments and doc comments — they're allowed to
                // mention forbidden tokens for documentation purposes.
                if trimmed.starts_with("//") {
                    continue;
                }
                for token in FORBIDDEN_TOKENS {
                    if line.contains(token) {
                        violations.push(format!(
                            "{}:{}: forbidden token `{}` outside client.rs",
                            path.display(),
                            idx + 1,
                            token
                        ));
                    }
                }
            }
        }

        assert!(
            violations.is_empty(),
            "kube::Api / mutation methods must be confined to src/k8s/client.rs:\n{}",
            violations.join("\n")
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
        let ar = discovery::resolve(&client, "pod")
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
        eprintln!("live_smoke: discovered {} pod(s) cluster-wide", pods.items.len());

        // Slow-path resolve: pick a kind that's not in the shortname table.
        // `Lease` from coordination.k8s.io exists on every modern cluster.
        let lease = discovery::resolve(&client, "Lease")
            .await
            .expect("resolve(Lease) via slow path failed");
        assert_eq!(lease.kind, "Lease");
        eprintln!(
            "live_smoke: slow-path resolved Lease -> {}/{}",
            lease.group, lease.version
        );
    }
}
