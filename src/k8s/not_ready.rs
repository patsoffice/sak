//! `sak k8s not-ready <kind>` — list objects whose target condition is not at
//! the wanted status.
//!
//! Generalizes pod triage to any condition-bearing kind. The
//! `.status.conditions[]` pattern — find the element with `type == X`, read its
//! `status`/`reason`/`message` — is how Flux (Kustomization, HelmRelease),
//! cert-manager (Certificate), Deployments, and most CRDs report health.
//!
//! Output is `namespace/name<TAB>status<TAB>reason<TAB>message`, sorted by
//! `(namespace, name)`. A row is emitted when the named condition's status is
//! not the wanted value, or when the condition is absent entirely. Missing
//! `status`/`reason`/`message` render as `-`.

use crate::output::Outcome;
use std::io;

use anyhow::{Result, bail};
use clap::Args;
use kube::api::ListParams;
use kube::discovery::Scope;
use serde_json::Value;

use crate::k8s::{client, discovery};
use crate::output::{BoundedWriter, collapse_newlines};

#[derive(Args)]
#[command(
    about = "List objects whose target condition is not at the wanted status",
    long_about = "List objects of <kind> whose `.status.conditions[]` element of \
        type --condition is not at the --status value (or is absent entirely).\n\n\
        This is the standard condition convention used by Flux (Kustomization, \
        HelmRelease), cert-manager (Certificate), Deployments, and most CRDs. \
        Defaults (`--condition Ready --status True`) cover Flux and \
        cert-manager; pass `--condition Available` for Deployments, etc.\n\n\
        Output is `namespace/name<TAB>status<TAB>reason<TAB>message`, sorted by \
        (namespace, name). An object with the condition at the wanted status is \
        omitted; a missing condition counts as not-ready. Exit code is 0 when \
        any object matches, 1 when none do (standard convention — not inverted).",
    after_help = "\
Examples:
  sak k8s not-ready kustomization -A                       Flux Kustomizations not Ready
  sak k8s not-ready helmrelease -A                         HelmReleases not Ready
  sak k8s not-ready deployment -A --condition Available    Deployments not Available
  sak k8s not-ready certificate -n cert-manager            Certificates not Ready in a namespace"
)]
pub struct NotReadyArgs {
    /// Kind to inspect (e.g. `kustomization`, `helmrelease`, `deployment`)
    pub kind: String,

    /// Namespace scope
    #[arg(short, long, conflicts_with = "all_namespaces")]
    pub namespace: Option<String>,

    /// List across all namespaces
    #[arg(short = 'A', long)]
    pub all_namespaces: bool,

    /// Label selector (e.g. `app=nginx`)
    #[arg(short = 'l', long)]
    pub selector: Option<String>,

    /// Condition type to inspect
    #[arg(long, default_value = "Ready")]
    pub condition: String,

    /// Healthy status value; rows are emitted where the actual status differs
    #[arg(long, default_value = "True")]
    pub status: String,

    /// Maximum number of output lines
    #[arg(long)]
    pub limit: Option<usize>,
}

struct NotReadyRow {
    namespace: String,
    name: String,
    status: String,
    reason: String,
    message: String,
}

/// Returns a row when `obj`'s `cond_type` condition is not at `want`, including
/// when the condition (or the whole status block) is absent. Returns `None`
/// when the condition is present and already at the wanted status.
fn unmet(obj: &Value, cond_type: &str, want: &str) -> Option<NotReadyRow> {
    let metadata = obj.get("metadata");
    let namespace = metadata
        .and_then(|m| m.get("namespace"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let name = metadata
        .and_then(|m| m.get("name"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();

    let condition = obj
        .get("status")
        .and_then(|s| s.get("conditions"))
        .and_then(Value::as_array)
        .and_then(|conds| {
            conds
                .iter()
                .find(|c| c.get("type").and_then(Value::as_str) == Some(cond_type))
        });

    match condition {
        Some(cond) => {
            let status = cond.get("status").and_then(Value::as_str).unwrap_or("-");
            if status == want {
                return None;
            }
            let reason = cond.get("reason").and_then(Value::as_str).unwrap_or("-");
            let message = cond.get("message").and_then(Value::as_str).unwrap_or("-");
            Some(NotReadyRow {
                namespace,
                name,
                status: status.to_string(),
                reason: reason.to_string(),
                message: collapse_newlines(message),
            })
        }
        // Condition absent (or no status block) counts as not-ready.
        None => Some(NotReadyRow {
            namespace,
            name,
            status: "-".to_string(),
            reason: "-".to_string(),
            message: "-".to_string(),
        }),
    }
}

pub async fn run(args: &NotReadyArgs) -> Result<Outcome> {
    let client = client::build_client().await?;
    let (ar, caps) = discovery::resolve(&client, &args.kind).await?;

    // Validate scope vs flags before issuing any list call.
    if matches!(caps.scope, Scope::Cluster) {
        if args.namespace.is_some() {
            bail!(
                "kind {:?} is cluster-scoped — --namespace is not valid for it",
                ar.kind
            );
        }
        if args.all_namespaces {
            bail!(
                "kind {:?} is cluster-scoped — --all-namespaces is not valid for it",
                ar.kind
            );
        }
    }

    let effective_ns: Option<String> = match caps.scope {
        Scope::Cluster => None,
        Scope::Namespaced => {
            if args.all_namespaces {
                None
            } else if let Some(ns) = &args.namespace {
                Some(ns.clone())
            } else {
                Some(client.default_namespace().to_string())
            }
        }
    };

    let mut lp = ListParams::default();
    if let Some(sel) = &args.selector {
        lp = lp.labels(sel);
    }

    let list = client::list_dyn(&client, &ar, effective_ns.as_deref(), &lp).await?;

    let mut rows: Vec<NotReadyRow> = Vec::new();
    for obj in &list.items {
        let value: Value = serde_json::to_value(obj)?;
        if let Some(row) = unmet(&value, &args.condition, &args.status) {
            rows.push(row);
        }
    }
    rows.sort_by(|a, b| {
        a.namespace
            .cmp(&b.namespace)
            .then_with(|| a.name.cmp(&b.name))
    });

    let stdout = io::stdout();
    let handle = stdout.lock();
    let mut writer = BoundedWriter::new(handle, args.limit);

    let mut wrote_any = false;
    for row in &rows {
        let nsname = if row.namespace.is_empty() {
            row.name.clone()
        } else {
            format!("{}/{}", row.namespace, row.name)
        };
        let line = format!(
            "{}\t{}\t{}\t{}",
            nsname, row.status, row.reason, row.message
        );
        if !writer.write_line(&line)? {
            break;
        }
        wrote_any = true;
    }

    writer.flush()?;
    if wrote_any {
        Ok(Outcome::Found)
    } else {
        Ok(Outcome::NotFound)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn ready_true_is_healthy() {
        let obj = json!({
            "metadata": {"namespace": "ns", "name": "k"},
            "status": {"conditions": [{"type": "Ready", "status": "True"}]}
        });
        assert!(unmet(&obj, "Ready", "True").is_none());
    }

    #[test]
    fn ready_false_is_not_ready() {
        let obj = json!({
            "metadata": {"namespace": "self-hosted", "name": "tracefinity"},
            "status": {"conditions": [{
                "type": "Ready",
                "status": "False",
                "reason": "ReconciliationFailed",
                "message": "ReplicationDestination must be of type string"
            }]}
        });
        let row = unmet(&obj, "Ready", "True").expect("not ready");
        assert_eq!(row.namespace, "self-hosted");
        assert_eq!(row.name, "tracefinity");
        assert_eq!(row.status, "False");
        assert_eq!(row.reason, "ReconciliationFailed");
        assert_eq!(row.message, "ReplicationDestination must be of type string");
    }

    #[test]
    fn condition_absent_is_not_ready() {
        let obj = json!({
            "metadata": {"namespace": "ns", "name": "k"},
            "status": {"conditions": [{"type": "Reconciling", "status": "True"}]}
        });
        let row = unmet(&obj, "Ready", "True").expect("not ready");
        assert_eq!(row.status, "-");
        assert_eq!(row.reason, "-");
        assert_eq!(row.message, "-");
    }

    #[test]
    fn no_status_block_is_not_ready() {
        let obj = json!({"metadata": {"namespace": "ns", "name": "k"}});
        let row = unmet(&obj, "Ready", "True").expect("not ready");
        assert_eq!(row.status, "-");
    }

    #[test]
    fn deployment_available_false_with_custom_flags() {
        let obj = json!({
            "metadata": {"namespace": "apps", "name": "api"},
            "status": {"conditions": [
                {"type": "Progressing", "status": "True"},
                {"type": "Available", "status": "False", "reason": "MinimumReplicasUnavailable"}
            ]}
        });
        // Default condition (Ready) is absent → not ready.
        assert!(unmet(&obj, "Ready", "True").is_some());
        // Targeting Available picks the right element.
        let row = unmet(&obj, "Available", "True").expect("not available");
        assert_eq!(row.status, "False");
        assert_eq!(row.reason, "MinimumReplicasUnavailable");
        assert_eq!(row.message, "-");
    }

    #[test]
    fn available_true_with_custom_flag_is_healthy() {
        let obj = json!({
            "metadata": {"namespace": "apps", "name": "api"},
            "status": {"conditions": [{"type": "Available", "status": "True"}]}
        });
        assert!(unmet(&obj, "Available", "True").is_none());
    }

    #[test]
    fn message_newlines_are_collapsed() {
        let obj = json!({
            "metadata": {"namespace": "ns", "name": "k"},
            "status": {"conditions": [{
                "type": "Ready",
                "status": "False",
                "message": "line one\nline two\r\nline three"
            }]}
        });
        let row = unmet(&obj, "Ready", "True").expect("not ready");
        assert!(!row.message.contains('\n'));
        assert!(!row.message.contains('\r'));
        assert_eq!(row.message, "line one line two  line three");
    }
}
