//! `sak k8s events` — list cluster events, newest first.
//!
//! The second-most-used debug command after `logs`. When something is broken,
//! the first question is "what does the apiserver have to say about it" — the
//! answer lives in the `events` stream. Use `--for kind/name` to filter to one
//! object without piping through `sak fs grep`.
//!
//! Output is `last<TAB>type<TAB>reason<TAB>kind/name<TAB>message`, sorted by
//! `lastTimestamp` *descending* (newest first — opposite of sak default, but
//! events are time-series and the LLM almost always wants the most recent
//! first). Falls back to `eventTime` then `firstTimestamp` when `lastTimestamp`
//! is null. Multi-line `message` fields are collapsed to a single line.
//!
//! Helpers ([`fetch_events_for`], [`format_event_row`]) are exposed
//! `pub(super)` so `sak k8s describe` can populate its events section without
//! duplicating event-list logic.

use std::cmp::Ordering;
use std::io;
use std::process::ExitCode;

use anyhow::{Result, anyhow};
use clap::Args;
use kube::api::ListParams;
use serde_json::Value;

use crate::k8s::{client, discovery};
use crate::output::BoundedWriter;

#[derive(Args)]
#[command(
    about = "List cluster events, newest first",
    long_about = "List events from the apiserver, sorted by `lastTimestamp` \
        descending (newest first). Falls back to `eventTime` then \
        `firstTimestamp` when `lastTimestamp` is unset.\n\n\
        Output is `last<TAB>type<TAB>reason<TAB>kind/name<TAB>message`. \
        Multi-line messages are collapsed to a single line so each event \
        stays one row.\n\n\
        Use `--for KIND/NAME` to filter to events whose `involvedObject` \
        matches one resource (case-insensitive kind, exact name).",
    after_help = "\
Examples:
  sak k8s events                              Recent events in current namespace
  sak k8s events -A --limit 20                Most recent 20 across the cluster
  sak k8s events --for pod/web-0 -n web       Events for a single pod
  sak k8s events -A --for deploy/api          Events for one deployment, anywhere"
)]
pub struct EventsArgs {
    /// Namespace scope (default: cluster default from kubeconfig)
    #[arg(short, long, conflicts_with = "all_namespaces")]
    pub namespace: Option<String>,

    /// List across all namespaces (mutually exclusive with --namespace)
    #[arg(short = 'A', long)]
    pub all_namespaces: bool,

    /// Filter to events whose involvedObject matches `KIND/NAME`
    #[arg(long = "for", value_name = "KIND/NAME")]
    pub for_object: Option<String>,

    /// Maximum number of output lines
    #[arg(long)]
    pub limit: Option<usize>,
}

/// One row of event info extracted from a `core/v1/Event`. Pure data so it can
/// be unit-tested on hand-built fixtures with no cluster.
pub(super) struct EventRow {
    pub last: String,
    pub event_type: String,
    pub reason: String,
    pub kind_name: String,
    pub message: String,
}

/// Pull a row from a single Event value. Always returns `Some` — missing
/// fields are rendered as `-` rather than dropped, so a malformed event still
/// shows up in the output (better than silently hiding it).
pub(super) fn extract_event_row(ev: &Value) -> EventRow {
    let last = ev
        .get("lastTimestamp")
        .and_then(Value::as_str)
        .or_else(|| ev.get("eventTime").and_then(Value::as_str))
        .or_else(|| ev.get("firstTimestamp").and_then(Value::as_str))
        .unwrap_or("-")
        .to_string();
    let event_type = ev
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("-")
        .to_string();
    let reason = ev
        .get("reason")
        .and_then(Value::as_str)
        .unwrap_or("-")
        .to_string();
    let inv = ev.get("involvedObject");
    let kind = inv
        .and_then(|i| i.get("kind"))
        .and_then(Value::as_str)
        .unwrap_or("");
    let name = inv
        .and_then(|i| i.get("name"))
        .and_then(Value::as_str)
        .unwrap_or("");
    let kind_name = if kind.is_empty() && name.is_empty() {
        "-".to_string()
    } else {
        format!("{}/{}", kind, name)
    };
    let message = collapse_newlines(ev.get("message").and_then(Value::as_str).unwrap_or(""));

    EventRow {
        last,
        event_type,
        reason,
        kind_name,
        message,
    }
}

/// Collapse `\n` and `\r` in `s` to spaces so multi-line strings stay on a
/// single output row. Implemented via `chars().map().collect()` rather than
/// `str::replace` because the chokepoint grep test in `client.rs` forbids
/// `.replace(` outside the chokepoint module (it would also catch
/// `kube::Api::replace`, the mutation method we're guarding against).
pub(super) fn collapse_newlines(s: &str) -> String {
    s.chars()
        .map(|c| if c == '\n' || c == '\r' { ' ' } else { c })
        .collect()
}

/// Format one row as the `last<TAB>type<TAB>reason<TAB>kind/name<TAB>message`
/// line that both `events` and `describe` emit.
pub(super) fn format_event_row(row: &EventRow) -> String {
    format!(
        "{}\t{}\t{}\t{}\t{}",
        row.last, row.event_type, row.reason, row.kind_name, row.message
    )
}

/// Sort rows by timestamp descending (newest first), with `kind/name` as a
/// stable tiebreaker so equal-timestamp events have a deterministic order.
pub(super) fn sort_rows_desc(rows: &mut [EventRow]) {
    rows.sort_by(|a, b| match b.last.cmp(&a.last) {
        Ordering::Equal => a.kind_name.cmp(&b.kind_name),
        other => other,
    });
}

/// Fetch the events for one object, filtered client-side.
///
/// Field selectors on `involvedObject.*` are unreliable across apiserver
/// versions, so this lists every event in `namespace` (or cluster-wide when
/// `namespace` is `None`) and filters in-process. Returns rows already sorted
/// newest-first via [`sort_rows_desc`].
pub(super) async fn fetch_events_for(
    client: &kube::Client,
    namespace: Option<&str>,
    kind: &str,
    name: &str,
) -> Result<Vec<EventRow>> {
    let (ar, _caps) = discovery::resolve(client, "event").await?;
    let lp = ListParams::default();
    let list = client::list_dyn(client, &ar, namespace, &lp).await?;
    let mut rows: Vec<EventRow> = Vec::new();
    for obj in &list.items {
        let value: Value = serde_json::to_value(obj)?;
        if event_matches(&value, kind, name) {
            rows.push(extract_event_row(&value));
        }
    }
    sort_rows_desc(&mut rows);
    Ok(rows)
}

/// Does this event's `involvedObject` match `(kind, name)`? Kind comparison is
/// case-insensitive (`pod` matches `Pod`); name comparison is exact.
fn event_matches(ev: &Value, kind: &str, name: &str) -> bool {
    let inv = ev.get("involvedObject");
    let ikind = inv
        .and_then(|i| i.get("kind"))
        .and_then(Value::as_str)
        .unwrap_or("");
    let iname = inv
        .and_then(|i| i.get("name"))
        .and_then(Value::as_str)
        .unwrap_or("");
    ikind.eq_ignore_ascii_case(kind) && iname == name
}

pub async fn run(args: &EventsArgs) -> Result<ExitCode> {
    let client = client::build_client().await?;
    let (ar, _caps) = discovery::resolve(&client, "event").await?;

    let effective_ns: Option<String> = if args.all_namespaces {
        None
    } else if let Some(ns) = &args.namespace {
        Some(ns.clone())
    } else {
        Some(client.default_namespace().to_string())
    };

    // Parse `--for KIND/NAME` once, canonicalizing the kind via the builtin
    // shortname table so `pod/foo` and `Pod/foo` both work without an
    // apiserver round trip.
    let for_filter: Option<(String, String)> = match &args.for_object {
        Some(raw) => {
            let (k, n) = raw
                .split_once('/')
                .ok_or_else(|| anyhow!("--for expects KIND/NAME (got {raw:?})"))?;
            if k.is_empty() || n.is_empty() {
                return Err(anyhow!("--for expects KIND/NAME (got {raw:?})"));
            }
            let canonical_kind = discovery::lookup_builtin(k)
                .map(|gvk| gvk.kind)
                .unwrap_or_else(|| k.to_string());
            Some((canonical_kind, n.to_string()))
        }
        None => None,
    };

    let lp = ListParams::default();
    let list = client::list_dyn(&client, &ar, effective_ns.as_deref(), &lp).await?;

    let mut rows: Vec<EventRow> = Vec::new();
    for obj in &list.items {
        let value: Value = serde_json::to_value(obj)?;
        if let Some((kind, name)) = &for_filter
            && !event_matches(&value, kind, name)
        {
            continue;
        }
        rows.push(extract_event_row(&value));
    }
    sort_rows_desc(&mut rows);

    let stdout = io::stdout();
    let handle = stdout.lock();
    let mut writer = BoundedWriter::new(handle, args.limit);

    let mut wrote_any = false;
    for row in &rows {
        if !writer.write_line(&format_event_row(row))? {
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
    fn extract_uses_last_timestamp_when_present() {
        let ev = json!({
            "lastTimestamp": "2026-04-09T12:00:00Z",
            "eventTime": "2026-04-08T12:00:00Z",
            "firstTimestamp": "2026-04-07T12:00:00Z",
            "type": "Warning",
            "reason": "FailedScheduling",
            "involvedObject": {"kind": "Pod", "name": "p"},
            "message": "0/3 nodes available"
        });
        let row = extract_event_row(&ev);
        assert_eq!(row.last, "2026-04-09T12:00:00Z");
        assert_eq!(row.event_type, "Warning");
        assert_eq!(row.reason, "FailedScheduling");
        assert_eq!(row.kind_name, "Pod/p");
        assert_eq!(row.message, "0/3 nodes available");
    }

    #[test]
    fn extract_falls_back_to_event_time() {
        let ev = json!({
            "eventTime": "2026-04-08T12:00:00Z",
            "firstTimestamp": "2026-04-07T12:00:00Z",
            "type": "Normal",
            "reason": "Pulled",
            "involvedObject": {"kind": "Pod", "name": "p"},
            "message": "ok"
        });
        let row = extract_event_row(&ev);
        assert_eq!(row.last, "2026-04-08T12:00:00Z");
    }

    #[test]
    fn extract_falls_back_to_first_timestamp() {
        let ev = json!({
            "firstTimestamp": "2026-04-07T12:00:00Z",
            "type": "Normal",
            "reason": "Created",
            "involvedObject": {"kind": "Pod", "name": "p"},
            "message": "ok"
        });
        let row = extract_event_row(&ev);
        assert_eq!(row.last, "2026-04-07T12:00:00Z");
    }

    #[test]
    fn extract_collapses_multiline_message() {
        let ev = json!({
            "lastTimestamp": "2026-04-09T12:00:00Z",
            "type": "Warning",
            "reason": "BackOff",
            "involvedObject": {"kind": "Pod", "name": "p"},
            "message": "line one\nline two\rline three"
        });
        let row = extract_event_row(&ev);
        assert_eq!(row.message, "line one line two line three");
    }

    #[test]
    fn extract_missing_fields_use_dashes() {
        let ev = json!({});
        let row = extract_event_row(&ev);
        assert_eq!(row.last, "-");
        assert_eq!(row.event_type, "-");
        assert_eq!(row.reason, "-");
        assert_eq!(row.kind_name, "-");
        assert_eq!(row.message, "");
    }

    #[test]
    fn sort_orders_newest_first() {
        let mut rows = vec![
            EventRow {
                last: "2026-04-07T00:00:00Z".into(),
                event_type: "Normal".into(),
                reason: "A".into(),
                kind_name: "Pod/a".into(),
                message: "".into(),
            },
            EventRow {
                last: "2026-04-09T00:00:00Z".into(),
                event_type: "Normal".into(),
                reason: "B".into(),
                kind_name: "Pod/b".into(),
                message: "".into(),
            },
            EventRow {
                last: "2026-04-08T00:00:00Z".into(),
                event_type: "Normal".into(),
                reason: "C".into(),
                kind_name: "Pod/c".into(),
                message: "".into(),
            },
        ];
        sort_rows_desc(&mut rows);
        assert_eq!(rows[0].kind_name, "Pod/b");
        assert_eq!(rows[1].kind_name, "Pod/c");
        assert_eq!(rows[2].kind_name, "Pod/a");
    }

    #[test]
    fn sort_breaks_ties_by_kind_name() {
        let mut rows = vec![
            EventRow {
                last: "2026-04-09T00:00:00Z".into(),
                event_type: "Normal".into(),
                reason: "A".into(),
                kind_name: "Pod/zeta".into(),
                message: "".into(),
            },
            EventRow {
                last: "2026-04-09T00:00:00Z".into(),
                event_type: "Normal".into(),
                reason: "B".into(),
                kind_name: "Pod/alpha".into(),
                message: "".into(),
            },
        ];
        sort_rows_desc(&mut rows);
        assert_eq!(rows[0].kind_name, "Pod/alpha");
        assert_eq!(rows[1].kind_name, "Pod/zeta");
    }

    #[test]
    fn event_matches_kind_case_insensitive() {
        let ev = json!({"involvedObject": {"kind": "Pod", "name": "web-0"}});
        assert!(event_matches(&ev, "Pod", "web-0"));
        assert!(event_matches(&ev, "pod", "web-0"));
        assert!(event_matches(&ev, "POD", "web-0"));
        assert!(!event_matches(&ev, "Pod", "web-1"));
        assert!(!event_matches(&ev, "Service", "web-0"));
    }

    #[test]
    fn format_emits_tab_separated_line() {
        let row = EventRow {
            last: "2026-04-09T12:00:00Z".into(),
            event_type: "Warning".into(),
            reason: "FailedScheduling".into(),
            kind_name: "Pod/p".into(),
            message: "0/3 nodes".into(),
        };
        assert_eq!(
            format_event_row(&row),
            "2026-04-09T12:00:00Z\tWarning\tFailedScheduling\tPod/p\t0/3 nodes"
        );
    }
}
