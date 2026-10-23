//! `sak k8s owners <kind> <name>` — walk ownerReferences up to the root.
//!
//! Fetches the named resource, reads its first `metadata.ownerReferences`
//! entry, resolves that owner through discovery (builtin fast path when the
//! kind and group/version match, slow path otherwise), fetches it, and
//! repeats. One line per ancestor (`kind/name`), immediate parent first.
//! Stops at the first resource with no ownerReferences, at a missing owner
//! (emitting what was found), or after 8 hops to bound pathological loops.

use crate::output::Outcome;
use std::io;

use anyhow::{Context, Result};
use clap::Args;
use kube::api::ApiResource;
use kube::core::GroupVersionKind;
use kube::discovery::Scope;
use serde_json::Value;

use crate::k8s::{client, discovery};
use crate::output::BoundedWriter;

/// Maximum owner hops before giving up (loop defense).
const MAX_HOPS: usize = 8;

#[derive(Args)]
#[command(
    about = "Walk ownerReferences up to the root owner",
    long_about = "Walk `metadata.ownerReferences` from the named resource up to the \
        root — answering 'where does this pod come from' without chaining `get` calls.\n\n\
        Output is one `kind/name` line per ancestor, immediate parent first. The \
        walk follows the first owner reference at each hop, stops at the first \
        resource with no ownerReferences (or a deleted owner, emitting what was \
        found), and caps at 8 hops. A resource with no owners at all is exit 1; \
        a missing start resource is also exit 1.",
    after_help = "\
Examples:
  sak k8s owners pod web-0 -n web         Pod -> ReplicaSet -> Deployment
  sak k8s owners deploy api -n api         Owners of a Deployment (often none)"
)]
pub struct OwnersArgs {
    /// Resource kind (e.g. `pod`, `deployment`)
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

/// The next hop up: (group, version, kind, name) from the first owner
/// reference, or `None` when the object has no ownerReferences (or the first
/// entry lacks kind/name). Pure so the walk logic is unit-testable.
fn next_hop(obj: &Value) -> Option<(String, String, String, String)> {
    let refs = obj.get("metadata")?.get("ownerReferences")?.as_array()?;
    let first = refs.first()?;
    let kind = first.get("kind")?.as_str()?;
    let name = first.get("name")?.as_str()?;
    let api_version = first
        .get("apiVersion")
        .and_then(Value::as_str)
        .unwrap_or("");
    let (group, version) = match api_version.split_once('/') {
        Some((g, v)) => (g.to_string(), v.to_string()),
        None => (String::new(), api_version.to_string()),
    };
    Some((group, version, kind.to_string(), name.to_string()))
}

/// Resolve an owner hop to its `ApiResource` and scope. The owner reference
/// carries the exact group/version/kind, so one pinned lookup is precise for
/// builtins and CRDs alike (a bare kind would be ambiguous if a CRD shared
/// its name).
async fn resolve_hop(
    client: &kube::Client,
    group: &str,
    version: &str,
    kind: &str,
) -> Result<(ApiResource, Scope)> {
    let gvk = GroupVersionKind::gvk(group, version, kind);
    let (ar, caps) = discovery::resolve_gvk(client, &gvk).await?;
    Ok((ar, caps.scope))
}

/// The namespace to fetch an owner in: none for a cluster-scoped owner
/// kind, the owned resource's namespace otherwise. Pure so the rule is
/// testable without a cluster.
fn owner_namespace(scope: &Scope, child_namespace: Option<&str>) -> Option<String> {
    match scope {
        Scope::Cluster => None,
        Scope::Namespaced => child_namespace.map(str::to_string),
    }
}

pub async fn run(args: &OwnersArgs) -> Result<Outcome> {
    let client = client::build_client().await?;
    let (ar, caps) = discovery::resolve(&client, &args.kind).await?;
    if matches!(caps.scope, Scope::Cluster) && args.namespace.is_some() {
        anyhow::bail!(
            "kind {:?} is cluster-scoped — --namespace is not valid for it",
            ar.kind
        );
    }
    // Owners live in the same namespace as the resource (cluster-scoped
    // resources resolve cluster-wide).
    let namespace: Option<String> = match caps.scope {
        Scope::Cluster => None,
        Scope::Namespaced => Some(
            args.namespace
                .clone()
                .unwrap_or_else(|| client.default_namespace().to_string()),
        ),
    };

    let mut current = match client::get_dyn(&client, &ar, namespace.as_deref(), &args.name).await? {
        Some(obj) => serde_json::to_value(obj)?,
        None => return Ok(Outcome::NotFound),
    };

    let mut ancestors: Vec<(String, String)> = Vec::new();
    for _ in 0..MAX_HOPS {
        let Some((group, version, kind, name)) = next_hop(&current) else {
            break;
        };
        let (hop_ar, hop_scope) = resolve_hop(&client, &group, &version, &kind)
            .await
            .with_context(|| format!("resolving owner {kind}/{name}"))?;
        // A cluster-scoped owner (e.g. the Node that owns a static mirror
        // pod) lives outside the namespace: looking it up there 404s, which
        // used to read as "owner deleted" and report no owners at all.
        let hop_ns = owner_namespace(&hop_scope, namespace.as_deref());
        match client::get_dyn(&client, &hop_ar, hop_ns.as_deref(), &name).await? {
            Some(obj) => {
                ancestors.push((hop_ar.kind.clone(), name));
                current = serde_json::to_value(obj)?;
            }
            // Owner deleted out from under the chain: report what we found.
            None => break,
        }
    }
    if ancestors.is_empty() {
        return Ok(Outcome::NotFound);
    }

    let stdout = io::stdout();
    let handle = stdout.lock();
    let mut writer = BoundedWriter::new(handle, args.limit);
    for (kind, name) in &ancestors {
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
    fn cluster_scoped_owners_are_fetched_outside_the_namespace() {
        // Regression: a mirror pod's owner is its Node; fetching the Node in
        // the pod's namespace 404'd and the walk reported no owners.
        assert_eq!(owner_namespace(&Scope::Cluster, Some("kube-system")), None);
        assert_eq!(
            owner_namespace(&Scope::Namespaced, Some("kube-system")).as_deref(),
            Some("kube-system")
        );
    }

    #[test]
    fn hop_reads_first_owner_reference() {
        let obj = json!({
            "metadata": {
                "ownerReferences": [
                    {"apiVersion": "apps/v1", "kind": "ReplicaSet", "name": "web-abc"},
                    {"apiVersion": "apps/v1", "kind": "Other", "name": "web-xyz"},
                ]
            }
        });
        assert_eq!(
            next_hop(&obj),
            Some((
                "apps".to_string(),
                "v1".to_string(),
                "ReplicaSet".to_string(),
                "web-abc".to_string()
            ))
        );
    }

    #[test]
    fn hop_handles_core_group_and_missing_refs() {
        // Core group has no `/` in apiVersion.
        let obj = json!({
            "metadata": {
                "ownerReferences": [{"apiVersion": "v1", "kind": "Node", "name": "n1"}]
            }
        });
        assert_eq!(
            next_hop(&obj),
            Some((
                "".to_string(),
                "v1".to_string(),
                "Node".to_string(),
                "n1".to_string()
            ))
        );
        assert_eq!(next_hop(&json!({"metadata": {}})), None);
        assert_eq!(next_hop(&json!({})), None);
        // First entry without kind/name is not a hop.
        assert_eq!(
            next_hop(&json!({"metadata": {"ownerReferences": [{"uid": "x"}]}})),
            None
        );
    }
}
