//! `sak k8s restarts` — list pod containers with at least N restarts.
//!
//! Walks pods, extracts per-container restart counts and the most recent
//! `lastState.terminated.reason`, sorts by restart count descending, and
//! emits `namespace/pod<TAB>container<TAB>restarts<TAB>last-reason`.

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
    about = "List pod containers with restarts",
    long_about = "List containers across pods that have restarted at least \
        --min times (default 1, so containers with zero restarts are excluded).\n\n\
        Output is `namespace/pod<TAB>container<TAB>restarts<TAB>last-reason`, \
        sorted by restart count descending. `last-reason` comes from \
        `status.containerStatuses[*].lastState.terminated.reason` (or `-` if \
        absent).",
    after_help = "\
Examples:
  sak k8s restarts                          Restart-flapping containers in current ns
  sak k8s restarts -A                       Across every namespace
  sak k8s restarts -A --min 5               Only containers with >=5 restarts
  sak k8s restarts -A -l app=api            Filter by label selector"
)]
pub struct RestartsArgs {
    /// Namespace scope
    #[arg(short, long, conflicts_with = "all_namespaces")]
    pub namespace: Option<String>,

    /// List across all namespaces
    #[arg(short = 'A', long)]
    pub all_namespaces: bool,

    /// Label selector (e.g. `app=nginx`)
    #[arg(short = 'l', long)]
    pub selector: Option<String>,

    /// Minimum restart count to include (default: 1)
    #[arg(long, default_value_t = 1)]
    pub min: u64,

    /// Maximum number of output lines
    #[arg(long)]
    pub limit: Option<usize>,
}

/// One row of restart info extracted from a pod's `status.containerStatuses`.
pub(super) struct RestartRow {
    pub namespace: String,
    pub pod: String,
    pub container: String,
    pub restarts: u64,
    pub last_reason: String,
}

/// Extract one [`RestartRow`] per container in `pod.status.containerStatuses`.
///
/// Pure function over `serde_json::Value` so it's unit-testable on hand-built
/// fixtures with no cluster. Shared with `failing.rs` for status-message
/// extraction by way of [`container_status_reason`].
pub(super) fn extract_restart_rows(pod: &Value) -> Vec<RestartRow> {
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

    let Some(statuses) = pod
        .get("status")
        .and_then(|s| s.get("containerStatuses"))
        .and_then(Value::as_array)
    else {
        return Vec::new();
    };

    let mut rows = Vec::with_capacity(statuses.len());
    for cs in statuses {
        let container = cs
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let restarts = cs.get("restartCount").and_then(Value::as_u64).unwrap_or(0);
        let last_reason = cs
            .get("lastState")
            .and_then(|ls| ls.get("terminated"))
            .and_then(|t| t.get("reason"))
            .and_then(Value::as_str)
            .unwrap_or("-")
            .to_string();
        rows.push(RestartRow {
            namespace: namespace.clone(),
            pod: pod_name.clone(),
            container,
            restarts,
            last_reason,
        });
    }
    rows
}

/// Walk a pod's `status.containerStatuses[*]` and return the first
/// waiting/terminated `reason` found, or `None` if none of the containers
/// have one. Used by `failing` as a fallback when `status.reason` is unset.
pub(super) fn container_status_reason(pod: &Value) -> Option<String> {
    let statuses = pod
        .get("status")
        .and_then(|s| s.get("containerStatuses"))
        .and_then(Value::as_array)?;
    for cs in statuses {
        let state = cs.get("state")?;
        if let Some(reason) = state
            .get("waiting")
            .and_then(|w| w.get("reason"))
            .and_then(Value::as_str)
        {
            return Some(reason.to_string());
        }
        if let Some(reason) = state
            .get("terminated")
            .and_then(|t| t.get("reason"))
            .and_then(Value::as_str)
        {
            return Some(reason.to_string());
        }
    }
    None
}

pub async fn run(args: &RestartsArgs) -> Result<ExitCode> {
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

    let mut rows: Vec<RestartRow> = Vec::new();
    for obj in &list.items {
        let value: Value = serde_json::to_value(obj)?;
        for row in extract_restart_rows(&value) {
            if row.restarts >= args.min {
                rows.push(row);
            }
        }
    }
    // Sort by restart count descending, then (namespace, pod, container) for
    // deterministic ties.
    rows.sort_by(|a, b| {
        b.restarts
            .cmp(&a.restarts)
            .then_with(|| a.namespace.cmp(&b.namespace))
            .then_with(|| a.pod.cmp(&b.pod))
            .then_with(|| a.container.cmp(&b.container))
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
        let line = format!(
            "{}\t{}\t{}\t{}",
            nsname, row.container, row.restarts, row.last_reason
        );
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
    fn extract_rows_with_last_terminated_reason() {
        let pod = json!({
            "metadata": {"namespace": "default", "name": "web"},
            "status": {
                "containerStatuses": [
                    {
                        "name": "app",
                        "restartCount": 7,
                        "lastState": {"terminated": {"reason": "OOMKilled"}}
                    },
                    {
                        "name": "sidecar",
                        "restartCount": 0
                    }
                ]
            }
        });
        let rows = extract_restart_rows(&pod);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].namespace, "default");
        assert_eq!(rows[0].pod, "web");
        assert_eq!(rows[0].container, "app");
        assert_eq!(rows[0].restarts, 7);
        assert_eq!(rows[0].last_reason, "OOMKilled");
        assert_eq!(rows[1].container, "sidecar");
        assert_eq!(rows[1].restarts, 0);
        assert_eq!(rows[1].last_reason, "-");
    }

    #[test]
    fn extract_rows_missing_status_yields_nothing() {
        let pod = json!({"metadata": {"namespace": "ns", "name": "p"}});
        assert!(extract_restart_rows(&pod).is_empty());
    }

    #[test]
    fn container_status_reason_prefers_waiting() {
        let pod = json!({
            "status": {
                "containerStatuses": [{
                    "name": "c",
                    "state": {"waiting": {"reason": "CrashLoopBackOff"}}
                }]
            }
        });
        assert_eq!(
            container_status_reason(&pod),
            Some("CrashLoopBackOff".to_string())
        );
    }

    #[test]
    fn container_status_reason_falls_back_to_terminated() {
        let pod = json!({
            "status": {
                "containerStatuses": [{
                    "name": "c",
                    "state": {"terminated": {"reason": "Error"}}
                }]
            }
        });
        assert_eq!(container_status_reason(&pod), Some("Error".to_string()));
    }

    #[test]
    fn container_status_reason_none_when_running() {
        let pod = json!({
            "status": {
                "containerStatuses": [{
                    "name": "c",
                    "state": {"running": {"startedAt": "2024-01-01T00:00:00Z"}}
                }]
            }
        });
        assert_eq!(container_status_reason(&pod), None);
    }
}
