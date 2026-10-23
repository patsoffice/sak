//! `sak k8s can-i <verb> <resource>` — RBAC self-check.
//!
//! Wraps the `SelfSubjectAccessReview` API (`kubectl auth can-i`). Prints
//! `yes` (exit 0) or `no` (exit 1), with the apiserver's reason on stderr
//! when denied.

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
        `kubectl auth can-i` for sak. Prints `yes` (exit 0) or `no` (exit 1); \
        when denied, the apiserver's reason (if any) goes to stderr as \
        `reason: ...`.\n\n\
        `<resource>` is `resource[.group]` (`pods`, `deployments.apps`, \
        `ingresses.networking.k8s.io`), optionally with a subresource after a \
        slash (`pods/log`) — or pass `--subresource` and `--name` explicitly. \
        Omit `--namespace` for a cluster-scoped check.\n\n\
        Read-only with respect to cluster state: the underlying API call is a \
        `create` on the virtual SelfSubjectAccessReview resource, which the \
        apiserver never persists.",
    after_help = "\
Examples:
  sak k8s can-i list pods -n default      May I list pods here?
  sak k8s can-i delete nodes              May I delete nodes? (cluster-wide)
  sak k8s can-i get pods/log -n default   Subresource check via slash form"
)]
pub struct CanIArgs {
    /// Verb to check (e.g. `get`, `list`, `create`, `delete`, `*`)
    pub verb: String,

    /// Resource as `resource[.group]` with optional `/subresource`
    pub resource: String,

    /// Namespace scope (omit for a cluster-scoped check)
    #[arg(short, long)]
    pub namespace: Option<String>,

    /// Subresource (conflicts with the `resource/sub` slash form)
    #[arg(long)]
    pub subresource: Option<String>,

    /// Check on a specific resource name
    #[arg(long)]
    pub name: Option<String>,

    /// Maximum number of output lines
    #[arg(long)]
    pub limit: Option<usize>,
}

/// Parsed resource target: an API group (None = core) plus resource,
/// subresource, and name slots. Pure so the parsing rules are unit-testable.
struct Target {
    group: Option<String>,
    resource: String,
    subresource: Option<String>,
    name: Option<String>,
}

fn parse_resource(
    resource: &str,
    flag_subresource: Option<&str>,
    name: Option<&str>,
) -> Result<Target> {
    let (res_part, slash_sub) = match resource.split_once('/') {
        Some((r, s)) => (r, Some(s)),
        None => (resource, None),
    };
    if res_part.is_empty() {
        bail!("invalid resource {resource:?}");
    }
    let subresource = match (slash_sub, flag_subresource) {
        (Some(_), Some(_)) => {
            bail!("cannot specify a subresource both as `resource/sub` and with --subresource");
        }
        (Some(s), None) => Some(s.to_string()),
        (None, Some(s)) => Some(s.to_string()),
        (None, None) => None,
    };
    if let Some(s) = &subresource
        && s.is_empty()
    {
        bail!("invalid resource {resource:?}");
    }
    let (res, group) = match res_part.split_once('.') {
        Some((r, g)) if !r.is_empty() && !g.is_empty() => (r.to_string(), Some(g.to_string())),
        _ if !res_part.contains('.') => (res_part.to_string(), None),
        _ => bail!("invalid resource {resource:?}"),
    };
    Ok(Target {
        group,
        resource: res,
        subresource,
        name: name.map(str::to_string),
    })
}

pub async fn run(args: &CanIArgs) -> Result<Outcome> {
    let client = client::build_client().await?;
    let target = parse_resource(
        &args.resource,
        args.subresource.as_deref(),
        args.name.as_deref(),
    )?;
    let (allowed, reason) = client::can_i(
        &client,
        &args.verb,
        target.group.as_deref(),
        &target.resource,
        target.subresource.as_deref(),
        args.namespace.as_deref(),
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

    fn parse(resource: &str, sub: Option<&str>) -> Result<Target> {
        parse_resource(resource, sub, None)
    }

    #[test]
    fn bare_resource_is_core_group() {
        let t = parse("pods", None).unwrap();
        assert_eq!(t.group, None);
        assert_eq!(t.resource, "pods");
        assert_eq!(t.subresource, None);
    }

    #[test]
    fn dotted_resource_splits_group_at_first_dot() {
        let t = parse("deployments.apps", None).unwrap();
        assert_eq!(t.group.as_deref(), Some("apps"));
        assert_eq!(t.resource, "deployments");
        let t = parse("ingresses.networking.k8s.io", None).unwrap();
        assert_eq!(t.group.as_deref(), Some("networking.k8s.io"));
    }

    #[test]
    fn slash_form_sets_subresource() {
        let t = parse("pods/log", None).unwrap();
        assert_eq!(t.resource, "pods");
        assert_eq!(t.subresource.as_deref(), Some("log"));
    }

    #[test]
    fn flag_sets_subresource() {
        let t = parse("pods", Some("status")).unwrap();
        assert_eq!(t.subresource.as_deref(), Some("status"));
    }

    #[test]
    fn slash_and_flag_conflict() {
        assert!(parse("pods/log", Some("status")).is_err());
    }

    #[test]
    fn empty_parts_rejected() {
        assert!(parse("", None).is_err());
        assert!(parse("/log", None).is_err());
        assert!(parse("pods/", None).is_err());
        assert!(parse(".apps", None).is_err());
        assert!(parse("pods.", None).is_err());
    }
}
