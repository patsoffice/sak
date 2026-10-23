//! `sak k8s children <kind> <name>` — list immediate children by owner uid.
//!
//! Fetches the parent for its `metadata.uid`, then lists each likely-child
//! kind from [`child_kinds`] in the same namespace and keeps resources whose
//! `ownerReferences[].uid` matches. One level deep only — recursive descent
//! explodes on Deployments with many ReplicaSet revisions.

use crate::output::Outcome;
use std::io;

use anyhow::{Result, bail};
use clap::Args;
use kube::api::ListParams;
use kube::discovery::Scope;
use serde_json::Value;

use crate::k8s::{client, discovery};
use crate::output::BoundedWriter;

#[derive(Args)]
#[command(
    about = "List immediate children of a resource",
    long_about = "List the immediate children of a resource — answering 'what does \
        this Deployment own' without chaining `get` calls.\n\n\
        The parent is fetched for its `metadata.uid`; each likely-child kind \
        (see below) is listed in the same namespace and filtered to resources \
        whose `ownerReferences[].uid` matches. Output is one `kind/name` line \
        per child, sorted by (kind, name). One level deep only.\n\n\
        Known parent → child mappings: Deployment → ReplicaSet, StatefulSet → \
        Pod, DaemonSet → Pod, ReplicaSet → Pod, Job → Pod, CronJob → Job, \
        Service → EndpointSlice. Any other parent kind errors (exit 2). \
        Service children match by the `kubernetes.io/service-name` label, \
        since EndpointSlices carry no owner reference to their Service.",
    after_help = "\
Examples:
  sak k8s children deploy api -n api     ReplicaSets owned by the Deployment
  sak k8s children cronjob night -n ops  Jobs owned by the CronJob"
)]
pub struct ChildrenArgs {
    /// Resource kind (e.g. `deployment`, `cronjob`)
    pub kind: String,

    /// Resource name
    pub name: String,

    /// Namespace scope (default: cluster default from kubeconfig)
    #[arg(short, long)]
    pub namespace: Option<String>,

    /// Maximum number of output lines
    #[arg(long)]
    pub limit: Option<usize>,
}

/// Likely-child kinds for a parent kind (case-insensitive), or an error for
/// kinds with no known children. Pure so the table is unit-testable.
fn child_kinds(parent_kind: &str) -> Result<&'static [&'static str]> {
    match parent_kind.to_ascii_lowercase().as_str() {
        "deployment" | "deployments" => Ok(&["ReplicaSet"]),
        "statefulset" | "statefulsets" => Ok(&["Pod"]),
        "daemonset" | "daemonsets" => Ok(&["Pod"]),
        "replicaset" | "replicasets" => Ok(&["Pod"]),
        "job" | "jobs" => Ok(&["Pod"]),
        "cronjob" | "cronjobs" => Ok(&["Job"]),
        "service" | "services" => Ok(&["EndpointSlice"]),
        _ => bail!("no known child kinds for {parent_kind:?}"),
    }
}

/// True when `obj` is an EndpointSlice for `service_name`. Slices carry no
/// ownerReferences to their Service (verified live: the link is the
/// `kubernetes.io/service-name` label), so Service children match by label
/// while every controller parent matches by owner uid. Pure and
/// unit-testable.
fn is_service_slice(obj: &Value, service_name: &str) -> bool {
    obj.get("metadata")
        .and_then(|m| m.get("labels"))
        .and_then(|l| l.get("kubernetes.io/service-name"))
        .and_then(Value::as_str)
        .is_some_and(|s| s == service_name)
}

/// True when any of `obj`'s ownerReferences carries `uid`. Pure so the
/// uid-match rule is unit-testable without a cluster.
fn is_child_of(obj: &Value, uid: &str) -> bool {
    obj.get("metadata")
        .and_then(|m| m.get("ownerReferences"))
        .and_then(Value::as_array)
        .is_some_and(|refs| {
            refs.iter().any(|r| {
                r.get("uid")
                    .and_then(Value::as_str)
                    .is_some_and(|u| u == uid)
            })
        })
}

pub async fn run(args: &ChildrenArgs) -> Result<Outcome> {
    let client = client::build_client().await?;
    let (ar, caps) = discovery::resolve(&client, &args.kind).await?;
    if matches!(caps.scope, Scope::Cluster) && args.namespace.is_some() {
        bail!(
            "kind {:?} is cluster-scoped — --namespace is not valid for it",
            ar.kind
        );
    }
    let namespace: Option<String> = match caps.scope {
        Scope::Cluster => None,
        Scope::Namespaced => Some(
            args.namespace
                .clone()
                .unwrap_or_else(|| client.default_namespace().to_string()),
        ),
    };

    let parent = match client::get_dyn(&client, &ar, namespace.as_deref(), &args.name).await? {
        Some(obj) => serde_json::to_value(obj)?,
        None => return Ok(Outcome::NotFound),
    };
    let Some(uid) = parent
        .get("metadata")
        .and_then(|m| m.get("uid"))
        .and_then(Value::as_str)
    else {
        bail!("resource {}/{} has no metadata.uid", ar.kind, args.name);
    };

    let mut rows: Vec<(String, String)> = Vec::new();
    for child_kind in child_kinds(&ar.kind)? {
        let (child_ar, _caps) = discovery::resolve(&client, child_kind).await?;
        // EndpointSlices link to their Service by label, not owner uid.
        let by_label = child_ar.kind == "EndpointSlice";
        let list = client::list_dyn(
            &client,
            &child_ar,
            namespace.as_deref(),
            &ListParams::default(),
        )
        .await?;
        for obj in &list.items {
            let value: Value = serde_json::to_value(obj)?;
            let keep = if by_label {
                is_service_slice(&value, &args.name)
            } else {
                is_child_of(&value, uid)
            };
            if !keep {
                continue;
            }
            let name = value
                .get("metadata")
                .and_then(|m| m.get("name"))
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            rows.push((child_ar.kind.clone(), name));
        }
    }
    rows.sort();
    if rows.is_empty() {
        return Ok(Outcome::NotFound);
    }

    let stdout = io::stdout();
    let handle = stdout.lock();
    let mut writer = BoundedWriter::new(handle, args.limit);
    for (kind, name) in &rows {
        if !writer.write_line(&format!("{kind}/{name}"))? {
            break;
        }
    }
    writer.flush()?;
    Ok(Outcome::Found)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn child_kinds_table_matches_singular_and_plural() {
        assert_eq!(child_kinds("Deployment").unwrap(), ["ReplicaSet"]);
        assert_eq!(child_kinds("deployments").unwrap(), ["ReplicaSet"]);
        assert_eq!(child_kinds("CronJob").unwrap(), ["Job"]);
        assert_eq!(child_kinds("service").unwrap(), ["EndpointSlice"]);
        assert!(child_kinds("POD").is_err());
    }

    #[test]
    fn child_kinds_rejects_unknown() {
        let err = child_kinds("Widget").unwrap_err().to_string();
        assert!(err.contains("no known child kinds"), "{err}");
        assert!(err.contains("Widget"), "{err}");
    }

    #[test]
    fn is_service_slice_matches_label() {
        let labeled = json!({
            "metadata": {"labels": {"kubernetes.io/service-name": "web"}}
        });
        assert!(is_service_slice(&labeled, "web"));
        assert!(!is_service_slice(&labeled, "api"));
        assert!(!is_service_slice(&json!({"metadata": {}}), "web"));
        assert!(!is_service_slice(&json!({}), "web"));
    }

    #[test]
    fn is_child_of_matches_any_owner_uid() {
        let obj = json!({
            "metadata": {
                "ownerReferences": [
                    {"uid": "aaa", "kind": "ReplicaSet"},
                    {"uid": "bbb", "kind": "Deployment"},
                ]
            }
        });
        assert!(is_child_of(&obj, "bbb"));
        assert!(!is_child_of(&obj, "ccc"));
        assert!(!is_child_of(&json!({"metadata": {}}), "bbb"));
        assert!(!is_child_of(&json!({}), "bbb"));
    }
}
