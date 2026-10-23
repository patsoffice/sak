//! `sak k8s can-i <verb> <resource>` — RBAC self-check.
//!
//! Wraps the `SelfSubjectAccessReview` API (`kubectl auth can-i`). Prints
//! `yes` (exit 0) or `no` (exit 1), with the apiserver's reason on stderr
//! when denied. Arguments mean what they mean to kubectl — the agent hook
//! redirects `kubectl auth can-i` here, so the two must give the same answer.

use crate::output::Outcome;
use std::io;

use anyhow::{Result, bail};
use clap::Args;

use crate::k8s::client;
use crate::output::BoundedWriter;

#[derive(Args)]
#[command(
    about = "Check whether you may perform an action",
    long_about = "Ask the apiserver whether the current user may perform an action — \
        `kubectl auth can-i` for sak, with the same argument meanings. Prints `yes` \
        (exit 0) or `no` (exit 1); when denied, the apiserver's reason (if any) \
        goes to stderr as `reason: ...`.\n\n\
        `<resource>` is `resource[.group]` (`pods`, `deployments.apps`, \
        `ingresses.networking.k8s.io`), optionally followed by `/name` for one \
        object (`pods/web-0`), as in kubectl. Check a subresource with \
        `--subresource` (`--subresource log`).\n\n\
        Like kubectl, the check applies to the current namespace (from \
        kubeconfig) unless `-n` names one; `-A` asks about all namespaces \
        (a cluster-wide grant). Non-resource URLs (`/healthz`) are not \
        supported.\n\n\
        Read-only with respect to cluster state: the underlying API call is a \
        `create` on the virtual SelfSubjectAccessReview resource, which the \
        apiserver never persists.",
    after_help = "\
Examples:
  sak k8s can-i list pods                       In the current namespace
  sak k8s can-i list pods -n kube-system        In another namespace
  sak k8s can-i delete nodes -A                 Cluster-wide
  sak k8s can-i get pods/web-0 -n web           One named object
  sak k8s can-i get pods --subresource log      A subresource"
)]
pub struct CanIArgs {
    /// Verb to check (e.g. `get`, `list`, `create`, `delete`, `*`)
    pub verb: String,

    /// Resource as `resource[.group]` with optional `/name`
    pub resource: String,

    /// Namespace to check in (default: current namespace from kubeconfig)
    #[arg(short, long, conflicts_with = "all_namespaces")]
    pub namespace: Option<String>,

    /// Check across all namespaces (a cluster-wide grant)
    #[arg(short = 'A', long)]
    pub all_namespaces: bool,

    /// Subresource to check (e.g. `log`, `status`)
    #[arg(long)]
    pub subresource: Option<String>,

    /// Check on a specific resource name (same as `resource/name`)
    #[arg(long)]
    pub name: Option<String>,

    /// Maximum number of output lines
    #[arg(long)]
    pub limit: Option<usize>,
}

/// Parsed resource target: an API group (None = core) plus resource,
/// subresource, and name slots. Pure so the parsing rules are unit-testable.
#[derive(Debug)]
struct Target {
    group: Option<String>,
    resource: String,
    subresource: Option<String>,
    name: Option<String>,
}

/// Parse kubectl's `TYPE[.group][/NAME]` form. The slash introduces a
/// resource *name* (`pods/web-0`), exactly as in `kubectl auth can-i`;
/// subresources come only from `--subresource`. A leading `/` is a
/// non-resource URL, which this command does not check.
fn parse_resource(
    resource: &str,
    subresource: Option<&str>,
    flag_name: Option<&str>,
) -> Result<Target> {
    if resource.starts_with('/') {
        bail!("non-resource URLs ({resource}) are not supported; use `kubectl auth can-i`");
    }
    let (res_part, slash_name) = match resource.split_once('/') {
        Some((r, n)) => (r, Some(n)),
        None => (resource, None),
    };
    if res_part.is_empty() || slash_name.is_some_and(str::is_empty) {
        bail!("invalid resource {resource:?}");
    }
    let name = match (slash_name, flag_name) {
        (Some(_), Some(_)) => bail!("cannot give a name both as `resource/name` and with --name"),
        (Some(n), None) | (None, Some(n)) => Some(n.to_string()),
        (None, None) => None,
    };
    if subresource.is_some_and(str::is_empty) {
        bail!("--subresource must not be empty");
    }
    let (res, group) = match res_part.split_once('.') {
        Some((r, g)) if !r.is_empty() && !g.is_empty() => (r.to_string(), Some(g.to_string())),
        _ if !res_part.contains('.') => (res_part.to_string(), None),
        _ => bail!("invalid resource {resource:?}"),
    };
    Ok(Target {
        group,
        resource: res,
        subresource: subresource.map(str::to_string),
        name,
    })
}

/// The namespace the review asks about: kubectl's rule, so the hook's
/// redirect gives the same answer. `-A` is cluster-wide (`None`); otherwise
/// `-n`, else the kubeconfig's current namespace.
fn review_namespace(
    all_namespaces: bool,
    namespace: Option<&str>,
    current: &str,
) -> Option<String> {
    if all_namespaces {
        None
    } else {
        Some(namespace.unwrap_or(current).to_string())
    }
}

pub async fn run(args: &CanIArgs) -> Result<Outcome> {
    let target = parse_resource(
        &args.resource,
        args.subresource.as_deref(),
        args.name.as_deref(),
    )?;
    let client = client::build_client().await?;
    let namespace = review_namespace(
        args.all_namespaces,
        args.namespace.as_deref(),
        client.default_namespace(),
    );
    let (allowed, reason) = client::can_i(
        &client,
        &args.verb,
        target.group.as_deref(),
        &target.resource,
        target.subresource.as_deref(),
        namespace.as_deref(),
        target.name.as_deref(),
    )
    .await?;
    if !allowed {
        if let Some(r) = reason.filter(|r| !r.is_empty()) {
            eprintln!("reason: {r}");
        }
        return Ok(Outcome::NotFound);
    }

    let stdout = io::stdout();
    let handle = stdout.lock();
    let mut writer = BoundedWriter::new(handle, args.limit);
    writer.write_line("yes")?;
    writer.flush()?;
    Ok(Outcome::Found)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(resource: &str) -> Result<Target> {
        parse_resource(resource, None, None)
    }

    #[test]
    fn bare_resource_is_core_group() {
        let t = parse("pods").unwrap();
        assert_eq!(t.group, None);
        assert_eq!(t.resource, "pods");
        assert_eq!(t.subresource, None);
        assert_eq!(t.name, None);
    }

    #[test]
    fn dotted_resource_splits_group_at_first_dot() {
        let t = parse("deployments.apps").unwrap();
        assert_eq!(t.group.as_deref(), Some("apps"));
        assert_eq!(t.resource, "deployments");
        let t = parse("ingresses.networking.k8s.io").unwrap();
        assert_eq!(t.group.as_deref(), Some("networking.k8s.io"));
    }

    #[test]
    fn slash_names_an_object_as_in_kubectl() {
        // Regression: the slash was read as a subresource, so
        // `kubectl auth can-i get pods/web-0` (redirected here) asked about
        // a `web-0` subresource instead of the pod named web-0.
        let t = parse("pods/web-0").unwrap();
        assert_eq!(t.resource, "pods");
        assert_eq!(t.name.as_deref(), Some("web-0"));
        assert_eq!(t.subresource, None);
        let t = parse("deployments.apps/api").unwrap();
        assert_eq!(t.group.as_deref(), Some("apps"));
        assert_eq!(t.name.as_deref(), Some("api"));
    }

    #[test]
    fn subresource_comes_from_the_flag() {
        let t = parse_resource("pods", Some("log"), None).unwrap();
        assert_eq!(t.subresource.as_deref(), Some("log"));
        assert!(parse_resource("pods", Some(""), None).is_err());
    }

    #[test]
    fn slash_and_name_flag_conflict() {
        assert!(parse_resource("pods/a", None, Some("b")).is_err());
        let t = parse_resource("pods", None, Some("b")).unwrap();
        assert_eq!(t.name.as_deref(), Some("b"));
    }

    #[test]
    fn non_resource_urls_and_empty_parts_rejected() {
        let err = parse("/healthz").unwrap_err().to_string();
        assert!(err.contains("non-resource"), "{err}");
        for bad in ["", "pods/", ".apps", "pods."] {
            assert!(parse(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn namespace_defaults_like_kubectl() {
        // Regression: omitting -n asked cluster-wide, so a user with
        // namespace-scoped RBAC got "no" where kubectl says "yes".
        assert_eq!(
            review_namespace(false, None, "team-a").as_deref(),
            Some("team-a")
        );
        assert_eq!(
            review_namespace(false, Some("prod"), "team-a").as_deref(),
            Some("prod")
        );
        assert_eq!(review_namespace(true, None, "team-a"), None);
    }
}
