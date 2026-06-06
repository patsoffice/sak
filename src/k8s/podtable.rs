//! Tabular rendering helpers for `sak k8s get -o table|wide`.
//!
//! Two concerns live here, both as pure functions over `serde_json::Value` (plus
//! an injected "now" for age), so they're unit-testable on hand-built fixtures
//! with no cluster:
//!
//! - **Pod column derivation** — READY (`ready/total` containers), STATUS
//!   (kubectl's `printPod` reason logic), RESTARTS (summed), IP, and NODE.
//! - **Age** — RFC 3339 (UTC, `…Z`) parsing and kubectl-style
//!   `HumanDuration` formatting (`5d`, `3h2m`, `2y34d`, …).
//!
//! The STATUS logic mirrors `pkg/printers/internalversion/printers.go::printPod`
//! in kubectl closely enough to match on the common cases (init-container
//! progress, waiting/terminated reasons, `Completed`-but-running, terminating).

use serde_json::Value;

/// Sum of `restartCount` across a pod's (non-init) container statuses, matching
/// the count kubectl shows in the RESTARTS column for a normally-running pod.
/// During init the count is taken from init-container statuses instead; that
/// case is folded into [`pod_status`] which also computes restarts, but the
/// table renderer keeps them separate for clarity and uses this for the common
/// path.
pub fn pod_restarts(pod: &Value) -> u64 {
    let init = container_statuses(pod, "initContainerStatuses");
    // If still initializing, kubectl reports init-container restarts.
    if let Some((_, initializing)) = init_reason(pod)
        && initializing
    {
        return sum_restarts(init);
    }
    sum_restarts(container_statuses(pod, "containerStatuses"))
}

fn sum_restarts(statuses: Option<&Vec<Value>>) -> u64 {
    statuses
        .map(|s| {
            s.iter()
                .map(|cs| cs.get("restartCount").and_then(Value::as_u64).unwrap_or(0))
                .sum()
        })
        .unwrap_or(0)
}

/// `ready/total` container string, e.g. `2/3`. `total` is the number of
/// containers in `spec.containers`; `ready` counts container statuses with
/// `ready == true`. Matches kubectl's READY column for pods.
pub fn pod_ready(pod: &Value) -> String {
    let total = pod
        .get("spec")
        .and_then(|s| s.get("containers"))
        .and_then(Value::as_array)
        .map(Vec::len)
        .unwrap_or(0);
    let ready = container_statuses(pod, "containerStatuses")
        .map(|s| {
            s.iter()
                .filter(|cs| cs.get("ready").and_then(Value::as_bool) == Some(true))
                .count()
        })
        .unwrap_or(0);
    format!("{ready}/{total}")
}

/// `status.hostIP`/`podIP`-style node placement: the node name a pod is
/// scheduled on (`spec.nodeName`), or `-` if unscheduled.
pub fn pod_node(pod: &Value) -> String {
    pod.get("spec")
        .and_then(|s| s.get("nodeName"))
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .unwrap_or("-")
        .to_string()
}

/// The pod's primary IP (`status.podIP`), or `-` if not yet assigned.
pub fn pod_ip(pod: &Value) -> String {
    pod.get("status")
        .and_then(|s| s.get("podIP"))
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .unwrap_or("-")
        .to_string()
}

fn container_statuses<'a>(pod: &'a Value, key: &str) -> Option<&'a Vec<Value>> {
    pod.get("status")
        .and_then(|s| s.get(key))
        .and_then(Value::as_array)
}

/// Compute the STATUS column the way kubectl's `printPod` does: scan init
/// containers first (returning an `Init:*` reason while initializing), then fall
/// back to the regular container states, the pod phase, and `status.reason`,
/// with the `Completed`-but-actually-running and terminating overrides applied
/// last.
pub fn pod_status(pod: &Value) -> String {
    let status = pod.get("status");
    let phase = status
        .and_then(|s| s.get("phase"))
        .and_then(Value::as_str)
        .unwrap_or("");
    let mut reason = status
        .and_then(|s| s.get("reason"))
        .and_then(Value::as_str)
        .filter(|r| !r.is_empty())
        .unwrap_or(phase)
        .to_string();

    let (init_reason, initializing) = init_reason(pod).unwrap_or((String::new(), false));
    if initializing {
        reason = init_reason;
    } else {
        let mut has_running = false;
        if let Some(statuses) = container_statuses(pod, "containerStatuses") {
            // kubectl walks container statuses in reverse so the first listed
            // container's reason wins on ties.
            for cs in statuses.iter().rev() {
                let state = cs.get("state");
                if let Some(r) = waiting_reason(state) {
                    reason = r;
                } else if let Some(r) = terminated_reason(state) {
                    reason = r;
                } else if cs.get("ready").and_then(Value::as_bool) == Some(true)
                    && state.and_then(|s| s.get("running")).is_some()
                {
                    has_running = true;
                }
            }
        }
        if reason == "Completed" && has_running {
            reason = if pod_ready_condition(pod) {
                "Running".to_string()
            } else {
                "NotReady".to_string()
            };
        }
    }

    // Deletion overrides everything.
    let deleting = pod
        .get("metadata")
        .and_then(|m| m.get("deletionTimestamp"))
        .is_some();
    if deleting {
        let node_lost =
            status.and_then(|s| s.get("reason")).and_then(Value::as_str) == Some("NodeLost");
        return if node_lost {
            "Unknown".to_string()
        } else {
            "Terminating".to_string()
        };
    }

    if reason.is_empty() {
        "-".to_string()
    } else {
        reason
    }
}

/// Walk init-container statuses and return `(reason, initializing)` if the pod
/// is still initializing (or has a failed init container), else `None`. Mirrors
/// the init-container branch of kubectl's `printPod`.
fn init_reason(pod: &Value) -> Option<(String, bool)> {
    let statuses = container_statuses(pod, "initContainerStatuses")?;
    let total = pod
        .get("spec")
        .and_then(|s| s.get("initContainers"))
        .and_then(Value::as_array)
        .map(Vec::len)
        .unwrap_or(statuses.len());

    for (i, cs) in statuses.iter().enumerate() {
        let state = cs.get("state");
        // Terminated cleanly → this init container is done; keep scanning.
        if let Some(term) = state.and_then(|s| s.get("terminated")) {
            if term.get("exitCode").and_then(Value::as_i64) == Some(0) {
                continue;
            }
            // Failed init container.
            let r = term
                .get("reason")
                .and_then(Value::as_str)
                .filter(|r| !r.is_empty())
                .map(|r| format!("Init:{r}"))
                .unwrap_or_else(|| match term.get("signal").and_then(Value::as_i64) {
                    Some(sig) if sig != 0 => format!("Init:Signal:{sig}"),
                    _ => format!(
                        "Init:ExitCode:{}",
                        term.get("exitCode").and_then(Value::as_i64).unwrap_or(0)
                    ),
                });
            return Some((r, true));
        }
        if let Some(r) = waiting_reason(state)
            && r != "PodInitializing"
        {
            return Some((format!("Init:{r}"), true));
        }
        // Still working through this init container.
        return Some((format!("Init:{i}/{total}"), true));
    }
    None
}

fn waiting_reason(state: Option<&Value>) -> Option<String> {
    state
        .and_then(|s| s.get("waiting"))
        .and_then(|w| w.get("reason"))
        .and_then(Value::as_str)
        .filter(|r| !r.is_empty())
        .map(str::to_string)
}

fn terminated_reason(state: Option<&Value>) -> Option<String> {
    let term = state.and_then(|s| s.get("terminated"))?;
    if let Some(r) = term
        .get("reason")
        .and_then(Value::as_str)
        .filter(|r| !r.is_empty())
    {
        return Some(r.to_string());
    }
    Some(match term.get("signal").and_then(Value::as_i64) {
        Some(sig) if sig != 0 => format!("Signal:{sig}"),
        _ => format!(
            "ExitCode:{}",
            term.get("exitCode").and_then(Value::as_i64).unwrap_or(0)
        ),
    })
}

fn pod_ready_condition(pod: &Value) -> bool {
    pod.get("status")
        .and_then(|s| s.get("conditions"))
        .and_then(Value::as_array)
        .map(|conds| {
            conds.iter().any(|c| {
                c.get("type").and_then(Value::as_str) == Some("Ready")
                    && c.get("status").and_then(Value::as_str) == Some("True")
            })
        })
        .unwrap_or(false)
}

/// Render a metadata `creationTimestamp` as a kubectl-style relative age given
/// the current unix time. A missing/unparseable timestamp renders as `-`.
pub fn translate_timestamp(creation: Option<&str>, now_unix: i64) -> String {
    let Some(ts) = creation.and_then(parse_rfc3339_utc) else {
        return "-".to_string();
    };
    human_duration(now_unix - ts)
}

/// Parse a Kubernetes RFC 3339 UTC timestamp (`YYYY-MM-DDTHH:MM:SS[.fff]Z`) into
/// a unix epoch second count. Returns `None` on any deviation from that shape —
/// k8s always emits UTC `…Z`, so we don't handle numeric offsets.
pub fn parse_rfc3339_utc(s: &str) -> Option<i64> {
    let s = s.strip_suffix('Z')?;
    let (date, time) = s.split_once('T')?;
    let mut d = date.split('-');
    let year: i64 = d.next()?.parse().ok()?;
    let month: i64 = d.next()?.parse().ok()?;
    let day: i64 = d.next()?.parse().ok()?;
    if d.next().is_some() {
        return None;
    }
    // Drop any fractional-second part.
    let time = time.split('.').next()?;
    let mut t = time.split(':');
    let hour: i64 = t.next()?.parse().ok()?;
    let min: i64 = t.next()?.parse().ok()?;
    let sec: i64 = t.next()?.parse().ok()?;
    if t.next().is_some() {
        return None;
    }
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    let days = days_from_civil(year, month, day);
    Some(days * 86400 + hour * 3600 + min * 60 + sec)
}

/// Days since the Unix epoch for a civil (proleptic Gregorian) date. Howard
/// Hinnant's `days_from_civil` algorithm — exact integer arithmetic, no leap
/// table.
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

/// kubectl's `duration.HumanDuration`: a compact, two-unit-at-most relative age
/// (`5s`, `3m`, `3m20s`, `5h`, `5h30m`, `7d`, `7d3h`, `2y`, `2y34d`). Negative
/// durations (clock skew) collapse to `0s`.
pub fn human_duration(secs: i64) -> String {
    if secs < -1 {
        return "<invalid>".to_string();
    }
    if secs < 0 {
        return "0s".to_string();
    }
    if secs < 60 * 2 {
        return format!("{secs}s");
    }
    let minutes = secs / 60;
    if minutes < 10 {
        let s = secs % 60;
        return if s == 0 {
            format!("{minutes}m")
        } else {
            format!("{minutes}m{s}s")
        };
    }
    if minutes < 60 * 3 {
        return format!("{minutes}m");
    }
    let hours = secs / 3600;
    if hours < 8 {
        let m = (secs / 60) % 60;
        return if m == 0 {
            format!("{hours}h")
        } else {
            format!("{hours}h{m}m")
        };
    }
    if hours < 48 {
        return format!("{hours}h");
    }
    if hours < 24 * 8 {
        let h = hours % 24;
        return if h == 0 {
            format!("{}d", hours / 24)
        } else {
            format!("{}d{}h", hours / 24, h)
        };
    }
    if hours < 24 * 365 * 2 {
        return format!("{}d", hours / 24);
    }
    if hours < 24 * 365 * 8 {
        let dy = hours / 24 / 365;
        let d = (hours / 24) % 365;
        return if d == 0 {
            format!("{dy}y")
        } else {
            format!("{dy}y{d}d")
        };
    }
    format!("{}y", hours / 24 / 365)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn ready_counts_ready_over_spec_total() {
        let pod = json!({
            "spec": {"containers": [{"name": "a"}, {"name": "b"}, {"name": "c"}]},
            "status": {"containerStatuses": [
                {"name": "a", "ready": true},
                {"name": "b", "ready": false},
                {"name": "c", "ready": true},
            ]}
        });
        assert_eq!(pod_ready(&pod), "2/3");
    }

    #[test]
    fn restarts_sum_across_containers() {
        let pod = json!({
            "status": {"containerStatuses": [
                {"name": "a", "restartCount": 3},
                {"name": "b", "restartCount": 4},
            ]}
        });
        assert_eq!(pod_restarts(&pod), 7);
    }

    #[test]
    fn status_running_from_phase() {
        let pod = json!({
            "spec": {"containers": [{"name": "a"}]},
            "status": {
                "phase": "Running",
                "containerStatuses": [
                    {"name": "a", "ready": true, "state": {"running": {"startedAt": "x"}}}
                ]
            }
        });
        assert_eq!(pod_status(&pod), "Running");
    }

    #[test]
    fn status_waiting_reason_wins() {
        let pod = json!({
            "status": {
                "phase": "Pending",
                "containerStatuses": [
                    {"name": "a", "state": {"waiting": {"reason": "CrashLoopBackOff"}}}
                ]
            }
        });
        assert_eq!(pod_status(&pod), "CrashLoopBackOff");
    }

    #[test]
    fn status_init_progress() {
        let pod = json!({
            "spec": {"initContainers": [{"name": "i0"}, {"name": "i1"}]},
            "status": {
                "phase": "Pending",
                "initContainerStatuses": [
                    {"name": "i0", "state": {"running": {}}}
                ]
            }
        });
        assert_eq!(pod_status(&pod), "Init:0/2");
    }

    #[test]
    fn status_init_failure_uses_reason() {
        let pod = json!({
            "status": {
                "phase": "Pending",
                "initContainerStatuses": [
                    {"name": "i0", "state": {"terminated": {"exitCode": 1, "reason": "Error"}}}
                ]
            }
        });
        assert_eq!(pod_status(&pod), "Init:Error");
    }

    #[test]
    fn status_completed_but_running_becomes_running_when_ready() {
        let pod = json!({
            "status": {
                "phase": "Running",
                "reason": "Completed",
                "conditions": [{"type": "Ready", "status": "True"}],
                "containerStatuses": [
                    {"name": "a", "ready": true, "state": {"running": {}}}
                ]
            }
        });
        // status.reason=Completed but a container is running & ready → Running.
        assert_eq!(pod_status(&pod), "Running");
    }

    #[test]
    fn status_terminating_on_deletion() {
        let pod = json!({
            "metadata": {"deletionTimestamp": "2024-01-01T00:00:00Z"},
            "status": {"phase": "Running"}
        });
        assert_eq!(pod_status(&pod), "Terminating");
    }

    #[test]
    fn status_unknown_on_node_lost_deletion() {
        let pod = json!({
            "metadata": {"deletionTimestamp": "2024-01-01T00:00:00Z"},
            "status": {"phase": "Running", "reason": "NodeLost"}
        });
        assert_eq!(pod_status(&pod), "Unknown");
    }

    #[test]
    fn node_and_ip_extraction() {
        let pod = json!({
            "spec": {"nodeName": "node-1"},
            "status": {"podIP": "10.0.0.5"}
        });
        assert_eq!(pod_node(&pod), "node-1");
        assert_eq!(pod_ip(&pod), "10.0.0.5");
        let empty = json!({"spec": {}, "status": {}});
        assert_eq!(pod_node(&empty), "-");
        assert_eq!(pod_ip(&empty), "-");
    }

    #[test]
    fn parse_timestamp_known_epoch() {
        // 2021-01-01T00:00:00Z == 1609459200
        assert_eq!(parse_rfc3339_utc("2021-01-01T00:00:00Z"), Some(1609459200));
        // Fractional seconds are dropped.
        assert_eq!(
            parse_rfc3339_utc("2021-01-01T00:00:00.123456Z"),
            Some(1609459200)
        );
        // Epoch itself.
        assert_eq!(parse_rfc3339_utc("1970-01-01T00:00:00Z"), Some(0));
        // Non-UTC / malformed.
        assert_eq!(parse_rfc3339_utc("2021-01-01T00:00:00+05:00"), None);
        assert_eq!(parse_rfc3339_utc("not-a-date"), None);
    }

    #[test]
    fn human_duration_tiers() {
        assert_eq!(human_duration(-5), "<invalid>");
        assert_eq!(human_duration(45), "45s");
        // 150s = 2m30s (< 10 minutes keeps the seconds component)
        assert_eq!(human_duration(150), "2m30s");
        // 180s = exactly 3m (zero seconds component drops the suffix)
        assert_eq!(human_duration(180), "3m");
        // 3m20s (< 10 minutes, non-zero seconds)
        assert_eq!(human_duration(3 * 60 + 20), "3m20s");
        // 90 minutes → 90m (< 3h)
        assert_eq!(human_duration(90 * 60), "90m");
        // 5h30m (< 8h)
        assert_eq!(human_duration(5 * 3600 + 30 * 60), "5h30m");
        // 30h → 30h (< 48h)
        assert_eq!(human_duration(30 * 3600), "30h");
        // 3 days 2 hours
        assert_eq!(human_duration(3 * 86400 + 2 * 3600), "3d2h");
        // 30 days → 30d
        assert_eq!(human_duration(30 * 86400), "30d");
        // 2 years 34 days
        assert_eq!(human_duration((2 * 365 + 34) * 86400), "2y34d");
    }

    #[test]
    fn translate_timestamp_missing_is_dash() {
        assert_eq!(translate_timestamp(None, 1_000_000), "-");
        assert_eq!(translate_timestamp(Some("garbage"), 1_000_000), "-");
    }

    #[test]
    fn translate_timestamp_computes_age() {
        // creation at epoch+0, now epoch+3 days → "3d"
        let age = translate_timestamp(Some("1970-01-01T00:00:00Z"), 3 * 86400);
        assert_eq!(age, "3d");
    }
}
