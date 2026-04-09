//! `sak k8s failing` — list pods whose phase is not Running or Succeeded.
//!
//! Output is `namespace/pod<TAB>phase<TAB>reason`, sorted by `(namespace, name)`.
//! `reason` is `status.reason` if set, else the most recent
//! waiting/terminated reason from any container in `status.containerStatuses`,
//! else `-`.

use std::io;
use std::process::ExitCode;

use anyhow::Result;
use clap::Args;
use kube::api::ListParams;
use serde_json::Value;

use crate::k8s::restarts::container_status_reason;
use crate::k8s::{client, discovery};
use crate::output::BoundedWriter;

#[derive(Args)]
#[command(
    about = "List pods that are not Running or Succeeded",
    long_about = "List pods whose `status.phase` is not in {Running, Succeeded}.\n\n\
        Output is `namespace/pod<TAB>phase<TAB>reason`, sorted by \
        (namespace, name). `reason` is `status.reason` if set, else the first \
        waiting/terminated reason found in `status.containerStatuses`, else `-`.",
    after_help = "\
Examples:
  sak k8s failing                       Failing pods in current namespace
  sak k8s failing -A                    Across every namespace
  sak k8s failing -A -l app=api         Filter by label selector"
)]
pub struct FailingArgs {
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

struct FailingRow {
    namespace: String,
    pod: String,
    phase: String,
    reason: String,
}

fn extract_failing_row(pod: &Value) -> Option<FailingRow> {
    let status = pod.get("status")?;
    let phase = status.get("phase").and_then(Value::as_str)?;
    if phase == "Running" || phase == "Succeeded" {
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
        .get("reason")
        .and_then(Value::as_str)
        .map(str::to_string)
        .or_else(|| container_status_reason(pod))
        .unwrap_or_else(|| "-".to_string());

    Some(FailingRow {
        namespace,
        pod: pod_name,
        phase: phase.to_string(),
        reason,
    })
}

pub async fn run(args: &FailingArgs) -> Result<ExitCode> {
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

    let mut rows: Vec<FailingRow> = Vec::new();
    for obj in &list.items {
        let value: Value = serde_json::to_value(obj)?;
        if let Some(row) = extract_failing_row(&value) {
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
        let line = format!("{}\t{}\t{}", nsname, row.phase, row.reason);
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
    fn running_pod_is_not_failing() {
        let pod = json!({
            "metadata": {"namespace": "ns", "name": "p"},
            "status": {"phase": "Running"}
        });
        assert!(extract_failing_row(&pod).is_none());
    }

    #[test]
    fn succeeded_pod_is_not_failing() {
        let pod = json!({
            "metadata": {"namespace": "ns", "name": "p"},
            "status": {"phase": "Succeeded"}
        });
        assert!(extract_failing_row(&pod).is_none());
    }

    #[test]
    fn pod_with_status_reason_uses_it_directly() {
        let pod = json!({
            "metadata": {"namespace": "ns", "name": "p"},
            "status": {"phase": "Failed", "reason": "Evicted"}
        });
        let row = extract_failing_row(&pod).expect("failing");
        assert_eq!(row.phase, "Failed");
        assert_eq!(row.reason, "Evicted");
    }

    #[test]
    fn pod_falls_back_to_container_status_reason() {
        let pod = json!({
            "metadata": {"namespace": "ns", "name": "p"},
            "status": {
                "phase": "Pending",
                "containerStatuses": [{
                    "name": "c",
                    "state": {"waiting": {"reason": "ImagePullBackOff"}}
                }]
            }
        });
        let row = extract_failing_row(&pod).expect("failing");
        assert_eq!(row.reason, "ImagePullBackOff");
    }

    #[test]
    fn pod_with_no_reason_uses_dash() {
        let pod = json!({
            "metadata": {"namespace": "ns", "name": "p"},
            "status": {"phase": "Pending"}
        });
        let row = extract_failing_row(&pod).expect("failing");
        assert_eq!(row.reason, "-");
    }
}
