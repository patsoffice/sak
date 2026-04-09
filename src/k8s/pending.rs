//! `sak k8s pending` — list pods stuck in the Pending phase, with the
//! scheduler's reason for why they can't be placed.
//!
//! Output is `namespace/pod<TAB>unschedulable-reason`, sorted by
//! `(namespace, name)`. The reason is the `message` from `status.conditions[*]`
//! where `type == "PodScheduled"` and `status == "False"`, else `-`.

use std::io;
use std::process::ExitCode;

use anyhow::Result;
use clap::Args;
use kube::api::ListParams;
use serde_json::Value;

use crate::k8s::{client, discovery};
use crate::output::BoundedWriter;

#[derive(Args)]
#[command(
    about = "List pods stuck in Pending",
    long_about = "List pods whose `status.phase == \"Pending\"`, with the \
        scheduler's unschedulable message extracted from the PodScheduled \
        condition.\n\n\
        Output is `namespace/pod<TAB>unschedulable-reason`, sorted by \
        (namespace, name). The reason is the `message` from \
        `status.conditions[*]` where `type == \"PodScheduled\"` and \
        `status == \"False\"`, else `-`.",
    after_help = "\
Examples:
  sak k8s pending                       Pending pods in current namespace
  sak k8s pending -A                    Across every namespace
  sak k8s pending -A -l app=api         Filter by label selector"
)]
pub struct PendingArgs {
    /// Namespace scope
    #[arg(short, long, conflicts_with = "all_namespaces")]
    pub namespace: Option<String>,

    /// List across all namespaces
    #[arg(short = 'A', long)]
    pub all_namespaces: bool,

    /// Label selector (e.g. `app=nginx`)
    #[arg(short = 'l', long)]
    pub selector: Option<String>,

    /// Maximum number of output lines
    #[arg(long)]
    pub limit: Option<usize>,
}

struct PendingRow {
    namespace: String,
    pod: String,
    reason: String,
}

fn extract_pending_row(pod: &Value) -> Option<PendingRow> {
    let status = pod.get("status")?;
    let phase = status.get("phase").and_then(Value::as_str)?;
    if phase != "Pending" {
        return None;
    }

    let metadata = pod.get("metadata");
    let namespace = metadata
        .and_then(|m| m.get("namespace"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let pod_name = metadata
        .and_then(|m| m.get("name"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();

    let reason = status
        .get("conditions")
        .and_then(Value::as_array)
        .and_then(|conds| {
            conds.iter().find_map(|c| {
                let ctype = c.get("type").and_then(Value::as_str)?;
                let cstatus = c.get("status").and_then(Value::as_str)?;
                if ctype == "PodScheduled" && cstatus == "False" {
                    c.get("message").and_then(Value::as_str).map(str::to_string)
                } else {
                    None
                }
            })
        })
        .unwrap_or_else(|| "-".to_string());

    Some(PendingRow {
        namespace,
        pod: pod_name,
        reason,
    })
}

pub async fn run(args: &PendingArgs) -> Result<ExitCode> {
    let client = client::build_client().await?;
    let (ar, _caps) = discovery::resolve(&client, "pod").await?;

    let effective_ns: Option<String> = if args.all_namespaces {
        None
    } else if let Some(ns) = &args.namespace {
        Some(ns.clone())
    } else {
        Some(client.default_namespace().to_string())
    };

    let mut lp = ListParams::default();
    if let Some(sel) = &args.selector {
        lp = lp.labels(sel);
    }

    let list = client::list_dyn(&client, &ar, effective_ns.as_deref(), &lp).await?;

    let mut rows: Vec<PendingRow> = Vec::new();
    for obj in &list.items {
        let value: Value = serde_json::to_value(obj)?;
        if let Some(row) = extract_pending_row(&value) {
            rows.push(row);
        }
    }
    rows.sort_by(|a, b| {
        a.namespace
            .cmp(&b.namespace)
            .then_with(|| a.pod.cmp(&b.pod))
    });

    let stdout = io::stdout();
    let handle = stdout.lock();
    let mut writer = BoundedWriter::new(handle, args.limit);

    let mut wrote_any = false;
    for row in &rows {
        let nsname = if row.namespace.is_empty() {
            row.pod.clone()
        } else {
            format!("{}/{}", row.namespace, row.pod)
        };
        let line = format!("{}\t{}", nsname, row.reason);
        if !writer.write_line(&line)? {
            break;
        }
        wrote_any = true;
    }

    writer.flush()?;
    if wrote_any {
        Ok(ExitCode::SUCCESS)
    } else {
        Ok(ExitCode::from(1))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn non_pending_pod_is_skipped() {
        let pod = json!({
            "metadata": {"namespace": "ns", "name": "p"},
            "status": {"phase": "Running"}
        });
        assert!(extract_pending_row(&pod).is_none());
    }

    #[test]
    fn pending_pod_with_pod_scheduled_false_extracts_message() {
        let pod = json!({
            "metadata": {"namespace": "ns", "name": "p"},
            "status": {
                "phase": "Pending",
                "conditions": [{
                    "type": "PodScheduled",
                    "status": "False",
                    "reason": "Unschedulable",
                    "message": "0/3 nodes are available: 3 Insufficient memory."
                }]
            }
        });
        let row = extract_pending_row(&pod).expect("pending");
        assert_eq!(row.namespace, "ns");
        assert_eq!(row.pod, "p");
        assert_eq!(
            row.reason,
            "0/3 nodes are available: 3 Insufficient memory."
        );
    }

    #[test]
    fn pending_pod_without_unschedulable_condition_uses_dash() {
        let pod = json!({
            "metadata": {"namespace": "ns", "name": "p"},
            "status": {
                "phase": "Pending",
                "conditions": [{
                    "type": "PodScheduled",
                    "status": "True"
                }]
            }
        });
        let row = extract_pending_row(&pod).expect("pending");
        assert_eq!(row.reason, "-");
    }

    #[test]
    fn pending_pod_with_no_conditions_uses_dash() {
        let pod = json!({
            "metadata": {"namespace": "ns", "name": "p"},
            "status": {"phase": "Pending"}
        });
        let row = extract_pending_row(&pod).expect("pending");
        assert_eq!(row.reason, "-");
    }
}
