//! `sak k8s describe <kind> <name>` — the "one command to look at a broken
//! thing" command.
//!
//! **Not `kubectl describe`.** kubectl's describe is human-prose oriented
//! (paragraphs, blank lines, mixed indentation) and is hostile to LLM parsing.
//! `sak k8s describe` outputs structured tab-separated sections with
//! `# Section` headers, the same way `sak git log` and `sak fs grep` already do.
//!
//! Sections, in order:
//!
//! 1. `# Object` — `kind/name`, namespace, age, labels (sorted), annotations
//! 2. `# Status` — `status.phase` and `status.conditions[*]`
//! 3. `# Containers` — declared containers from the resolved pod-template
//!    (via [`crate::k8s::containers::walk_containers`]), joined with
//!    `status.containerStatuses` for ready/restarts/last-state when present
//! 4. `# Owners` — `metadata.ownerReferences` chain walked up to 5 hops
//! 5. `# Events` — last 10 events whose `involvedObject` matches this
//!    resource, via [`crate::k8s::events::fetch_events_for`]
//!
//! Section headers are written as [`crate::output::BoundedWriter`] decorations
//! so they don't get truncated mid-section by `--limit`.

use std::io;
use std::process::ExitCode;

use anyhow::Result;
use clap::Args;
use kube::discovery::Scope;
use serde_json::Value;

use crate::k8s::containers::walk_containers;
use crate::k8s::events::{collapse_newlines, fetch_events_for, format_event_row};
use crate::k8s::{client, discovery};
use crate::output::BoundedWriter;

/// Maximum number of `ownerReferences` hops to walk before bailing — defends
/// against pathological loops in malformed cluster state.
const MAX_OWNER_HOPS: usize = 5;

/// Maximum number of events to include in the `# Events` section, regardless
/// of `--limit`. Tuned to fit on a screen and to keep one describe call from
/// flooding the LLM with hundreds of historical events.
const MAX_EVENTS: usize = 10;

#[derive(Args)]
#[command(
    about = "Aggregated description of one resource",
    long_about = "Fetch one resource and emit five LLM-friendly sections \
        describing its current state — object metadata, status, containers, \
        owner chain, and recent events. Designed to answer \"why is this thing \
        broken\" in a single call.\n\n\
        Output is structured: each section starts with a `# <Name>` header \
        line and the rows underneath are tab-separated. Section headers do \
        not count toward `--limit`.\n\n\
        Exit codes follow sak convention: 0 = found, 1 = not found, 2 = error.",
    after_help = "\
Examples:
  sak k8s describe pod web-0 -n web              Describe one pod
  sak k8s describe deploy api -n api             Describe a deployment
  sak k8s describe node worker-3                 Describe a cluster-scoped node
  sak k8s describe pod web-0 -n web --limit 50   Cap rows at 50"
)]
pub struct DescribeArgs {
    /// Resource kind (e.g. `pod`, `deployment`, `Lease`)
    pub kind: String,

    /// Resource name
    pub name: String,

    /// Namespace scope (default: cluster default from kubeconfig)
    #[arg(short, long)]
    pub namespace: Option<String>,

    /// Maximum number of output rows (section headers are not counted)
    #[arg(long)]
    pub limit: Option<usize>,
}

pub async fn run(args: &DescribeArgs) -> Result<ExitCode> {
    let client = client::build_client().await?;
    let (ar, caps) = discovery::resolve(&client, &args.kind).await?;

    // Resolve effective namespace using the same rules as `get`. Cluster-
    // scoped kinds ignore --namespace; namespaced kinds default to the
    // kubeconfig's current namespace when --namespace is omitted.
    let effective_ns: Option<String> = match caps.scope {
        Scope::Cluster => None,
        Scope::Namespaced => Some(
            args.namespace
                .clone()
                .unwrap_or_else(|| client.default_namespace().to_string()),
        ),
    };

    let obj = client::get_dyn(&client, &ar, effective_ns.as_deref(), &args.name).await?;
    let Some(obj) = obj else {
        return Ok(ExitCode::from(1));
    };
    let value: Value = serde_json::to_value(&obj)?;

    let stdout = io::stdout();
    let handle = stdout.lock();
    let mut writer = BoundedWriter::new(handle, args.limit);

    let kind_name = format!("{}/{}", ar.kind, args.name);

    writer.write_decoration("# Object")?;
    write_object_section(&mut writer, &value, &kind_name)?;

    writer.write_decoration("# Status")?;
    write_status_section(&mut writer, &value)?;

    writer.write_decoration("# Containers")?;
    write_containers_section(&mut writer, &value)?;

    writer.write_decoration("# Owners")?;
    write_owners_section(&mut writer, &client, &value, effective_ns.as_deref()).await?;

    writer.write_decoration("# Events")?;
    write_events_section(
        &mut writer,
        &client,
        effective_ns.as_deref(),
        &ar.kind,
        &args.name,
    )
    .await?;

    writer.flush()?;
    // The object was successfully fetched (404 returns earlier with exit 1),
    // so describe always exits SUCCESS once we get here — even if every
    // section happened to be empty, we still found the object itself.
    Ok(ExitCode::SUCCESS)
}

fn write_object_section(
    writer: &mut BoundedWriter<'_>,
    value: &Value,
    kind_name: &str,
) -> io::Result<()> {
    let metadata = value.get("metadata");
    let namespace = metadata
        .and_then(|m| m.get("namespace"))
        .and_then(Value::as_str);
    let creation_ts = metadata
        .and_then(|m| m.get("creationTimestamp"))
        .and_then(Value::as_str);

    if !writer.write_line(&format!("kind/name\t{kind_name}"))? {
        return Ok(());
    }
    if let Some(ns) = namespace
        && !writer.write_line(&format!("namespace\t{ns}"))?
    {
        return Ok(());
    }
    if let Some(ts) = creation_ts
        && !writer.write_line(&format!("age\t{}", humanize_age(ts)))?
    {
        return Ok(());
    }

    if let Some(labels) = metadata
        .and_then(|m| m.get("labels"))
        .and_then(Value::as_object)
    {
        let mut keys: Vec<&String> = labels.keys().collect();
        keys.sort();
        for k in keys {
            let v = labels.get(k).and_then(Value::as_str).unwrap_or("");
            if !writer.write_line(&format!("label\t{k}={v}"))? {
                return Ok(());
            }
        }
    }

    if let Some(anns) = metadata
        .and_then(|m| m.get("annotations"))
        .and_then(Value::as_object)
    {
        let mut keys: Vec<&String> = anns.keys().collect();
        keys.sort();
        for k in keys {
            let v = collapse_newlines(anns.get(k).and_then(Value::as_str).unwrap_or(""));
            if !writer.write_line(&format!("annotation\t{k}={v}"))? {
                return Ok(());
            }
        }
    }
    Ok(())
}

fn write_status_section(writer: &mut BoundedWriter<'_>, value: &Value) -> io::Result<()> {
    let Some(status) = value.get("status") else {
        return Ok(());
    };
    if let Some(phase) = status.get("phase").and_then(Value::as_str)
        && !writer.write_line(&format!("phase\t{phase}"))?
    {
        return Ok(());
    }
    if let Some(conds) = status.get("conditions").and_then(Value::as_array) {
        for c in conds {
            let ctype = c.get("type").and_then(Value::as_str).unwrap_or("-");
            let cstatus = c.get("status").and_then(Value::as_str).unwrap_or("-");
            let reason = c.get("reason").and_then(Value::as_str).unwrap_or("-");
            let message = collapse_newlines(c.get("message").and_then(Value::as_str).unwrap_or(""));
            if !writer.write_line(&format!("{ctype}\t{cstatus}\t{reason}\t{message}"))? {
                return Ok(());
            }
        }
    }
    Ok(())
}

fn write_containers_section(writer: &mut BoundedWriter<'_>, value: &Value) -> io::Result<()> {
    // Build a map of container name → status entry once, so we can join
    // declared (spec) containers with their runtime status (Pod only).
    let statuses: Vec<&Value> = value
        .get("status")
        .and_then(|s| s.get("containerStatuses"))
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[])
        .iter()
        .collect();

    for view in walk_containers(value) {
        let st = statuses
            .iter()
            .find(|s| s.get("name").and_then(Value::as_str) == Some(view.container));
        let (ready, restarts, last_state, last_reason) = match st {
            Some(s) => {
                let ready = s
                    .get("ready")
                    .and_then(Value::as_bool)
                    .map(|b| if b { "true" } else { "false" })
                    .unwrap_or("-")
                    .to_string();
                let restarts = s
                    .get("restartCount")
                    .and_then(Value::as_u64)
                    .map(|n| n.to_string())
                    .unwrap_or_else(|| "-".to_string());
                let last_state = last_state_label(s);
                let last_reason = last_terminated_reason(s);
                (ready, restarts, last_state, last_reason)
            }
            None => ("-".into(), "-".into(), "-".into(), "-".into()),
        };
        let line = format!(
            "{}\t{}\t{}\t{}\t{}\t{}",
            view.container, view.image, ready, restarts, last_state, last_reason
        );
        if !writer.write_line(&line)? {
            return Ok(());
        }
    }
    Ok(())
}

/// Pull the *kind* of `lastState` (waiting/running/terminated) from a
/// containerStatus, or `-` if absent.
fn last_state_label(cs: &Value) -> String {
    let Some(ls) = cs.get("lastState").and_then(Value::as_object) else {
        return "-".into();
    };
    for key in ["waiting", "running", "terminated"] {
        if ls.contains_key(key) {
            return key.into();
        }
    }
    "-".into()
}

/// Reason from `lastState.terminated.reason` (or waiting.reason if no
/// terminated entry exists), or `-`.
fn last_terminated_reason(cs: &Value) -> String {
    let ls = cs.get("lastState");
    if let Some(reason) = ls
        .and_then(|l| l.get("terminated"))
        .and_then(|t| t.get("reason"))
        .and_then(Value::as_str)
    {
        return reason.to_string();
    }
    if let Some(reason) = ls
        .and_then(|l| l.get("waiting"))
        .and_then(|w| w.get("reason"))
        .and_then(Value::as_str)
    {
        return reason.to_string();
    }
    "-".into()
}

async fn write_owners_section(
    writer: &mut BoundedWriter<'_>,
    client: &kube::Client,
    value: &Value,
    namespace: Option<&str>,
) -> Result<()> {
    let mut current = value.clone();
    let mut hops = 0;
    loop {
        if hops >= MAX_OWNER_HOPS {
            break;
        }
        let Some(refs) = current
            .get("metadata")
            .and_then(|m| m.get("ownerReferences"))
            .and_then(Value::as_array)
            .filter(|a| !a.is_empty())
            .cloned()
        else {
            break;
        };
        // Prefer the controller=true entry; fall back to the first if no
        // controller is marked.
        let owner = refs
            .iter()
            .find(|o| o.get("controller").and_then(Value::as_bool) == Some(true))
            .cloned()
            .unwrap_or_else(|| refs[0].clone());
        let owner_kind = owner
            .get("kind")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let owner_name = owner
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        if owner_kind.is_empty() || owner_name.is_empty() {
            break;
        }
        if !writer.write_line(&format!("{owner_kind}/{owner_name}"))? {
            return Ok(());
        }
        hops += 1;

        // Try to fetch the next ancestor so we can continue the chain. If we
        // can't resolve the kind or the apiserver returns 404, stop walking
        // — the row above is still recorded.
        let (ar, caps) = match discovery::resolve(client, &owner_kind).await {
            Ok(p) => p,
            Err(_) => break,
        };
        let owner_ns = match caps.scope {
            Scope::Cluster => None,
            Scope::Namespaced => namespace,
        };
        let next = match client::get_dyn(client, &ar, owner_ns, &owner_name).await {
            Ok(Some(o)) => o,
            _ => break,
        };
        current = serde_json::to_value(&next)?;
    }
    Ok(())
}

async fn write_events_section(
    writer: &mut BoundedWriter<'_>,
    client: &kube::Client,
    namespace: Option<&str>,
    kind: &str,
    name: &str,
) -> Result<()> {
    let rows = fetch_events_for(client, namespace, kind, name).await?;
    for row in rows.iter().take(MAX_EVENTS) {
        if !writer.write_line(&format_event_row(row))? {
            return Ok(());
        }
    }
    Ok(())
}

/// Convert an RFC3339-Z `creationTimestamp` to a coarse human-friendly age
/// string (`5d`, `3h`, `12m`, `4s`). Returns `-` if the timestamp can't be
/// parsed. We avoid pulling in `chrono`/`time` for what amounts to a single
/// human-friendly delta — the parser only needs to handle Kubernetes' fixed
/// `YYYY-MM-DDTHH:MM:SSZ` format.
fn humanize_age(ts: &str) -> String {
    let Some(then) = parse_rfc3339_utc_seconds(ts) else {
        return "-".into();
    };
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    if now < then {
        return "0s".into();
    }
    let dt = (now - then) as u64;
    if dt >= 86400 {
        format!("{}d", dt / 86400)
    } else if dt >= 3600 {
        format!("{}h", dt / 3600)
    } else if dt >= 60 {
        format!("{}m", dt / 60)
    } else {
        format!("{}s", dt)
    }
}

/// Parse a `YYYY-MM-DDTHH:MM:SS[.frac]Z` timestamp into Unix seconds.
///
/// Implementation uses Howard Hinnant's `days_from_civil` algorithm so we
/// don't need a date library. Only the UTC `Z` form is recognized — that's
/// the only form the apiserver emits for `creationTimestamp`.
fn parse_rfc3339_utc_seconds(s: &str) -> Option<i64> {
    if s.len() < 20 {
        return None;
    }
    let bytes = s.as_bytes();
    if bytes[4] != b'-'
        || bytes[7] != b'-'
        || bytes[10] != b'T'
        || bytes[13] != b':'
        || bytes[16] != b':'
    {
        return None;
    }
    let year: i64 = s[0..4].parse().ok()?;
    let month: u32 = s[5..7].parse().ok()?;
    let day: u32 = s[8..10].parse().ok()?;
    let hour: u32 = s[11..13].parse().ok()?;
    let minute: u32 = s[14..16].parse().ok()?;
    let second: u32 = s[17..19].parse().ok()?;
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }

    // days_from_civil
    let y = if month <= 2 { year - 1 } else { year };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = (y - era * 400) as u64; // [0, 399]
    let m = month as u64;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + day as u64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146097 + doe as i64 - 719468;

    Some(days * 86400 + hour as i64 * 3600 + minute as i64 * 60 + second as i64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_unix_epoch() {
        assert_eq!(parse_rfc3339_utc_seconds("1970-01-01T00:00:00Z"), Some(0));
    }

    #[test]
    fn parse_known_timestamp() {
        // 2024-01-01T00:00:00Z = 1704067200
        assert_eq!(
            parse_rfc3339_utc_seconds("2024-01-01T00:00:00Z"),
            Some(1_704_067_200)
        );
    }

    #[test]
    fn parse_with_fractional_seconds() {
        // We ignore the fractional part — should still parse the integer
        // seconds successfully.
        assert_eq!(
            parse_rfc3339_utc_seconds("2024-01-01T00:00:00.123Z"),
            Some(1_704_067_200)
        );
    }

    #[test]
    fn parse_rejects_bad_format() {
        assert_eq!(parse_rfc3339_utc_seconds("not a date"), None);
        assert_eq!(parse_rfc3339_utc_seconds("2024-01-01"), None);
        assert_eq!(parse_rfc3339_utc_seconds("2024/01/01T00:00:00Z"), None);
        assert_eq!(parse_rfc3339_utc_seconds("2024-13-01T00:00:00Z"), None);
    }

    #[test]
    fn humanize_age_handles_unparseable() {
        assert_eq!(humanize_age("not a date"), "-");
    }

    #[test]
    fn humanize_age_buckets_by_unit() {
        // We can't test against "now", but we can test the bucketing logic
        // by parsing known timestamps and checking format.
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        // 5 seconds ago
        let secs = format_unix_seconds(now - 5);
        assert!(humanize_age(&secs).ends_with('s'));
        // 5 minutes ago
        let secs = format_unix_seconds(now - 5 * 60);
        assert!(humanize_age(&secs).ends_with('m'));
        // 5 hours ago
        let secs = format_unix_seconds(now - 5 * 3600);
        assert!(humanize_age(&secs).ends_with('h'));
        // 5 days ago
        let secs = format_unix_seconds(now - 5 * 86400);
        assert!(humanize_age(&secs).ends_with('d'));
    }

    /// Test helper: convert Unix seconds back into an RFC3339 UTC string.
    /// Inverse of `parse_rfc3339_utc_seconds` for round-trip testing.
    fn format_unix_seconds(s: i64) -> String {
        // civil_from_days
        let z = s.div_euclid(86400) + 719468;
        let era = if z >= 0 { z } else { z - 146096 } / 146097;
        let doe = (z - era * 146097) as u64;
        let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
        let y = yoe as i64 + era * 400;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        let d = doy - (153 * mp + 2) / 5 + 1;
        let m = if mp < 10 { mp + 3 } else { mp - 9 };
        let year = if m <= 2 { y + 1 } else { y };
        let secs_of_day = s.rem_euclid(86400) as u64;
        let hh = secs_of_day / 3600;
        let mm = (secs_of_day / 60) % 60;
        let ss = secs_of_day % 60;
        format!("{year:04}-{m:02}-{d:02}T{hh:02}:{mm:02}:{ss:02}Z")
    }

    #[test]
    fn last_state_label_picks_first_present_key() {
        use serde_json::json;
        let cs = json!({"lastState": {"terminated": {"reason": "OOMKilled"}}});
        assert_eq!(last_state_label(&cs), "terminated");
        let cs = json!({"lastState": {"waiting": {"reason": "CrashLoopBackOff"}}});
        assert_eq!(last_state_label(&cs), "waiting");
        let cs = json!({"lastState": {}});
        assert_eq!(last_state_label(&cs), "-");
        let cs = json!({});
        assert_eq!(last_state_label(&cs), "-");
    }

    #[test]
    fn last_terminated_reason_prefers_terminated_over_waiting() {
        use serde_json::json;
        let cs = json!({"lastState": {
            "terminated": {"reason": "Error"},
            "waiting": {"reason": "CrashLoopBackOff"}
        }});
        assert_eq!(last_terminated_reason(&cs), "Error");
        let cs = json!({"lastState": {"waiting": {"reason": "CrashLoopBackOff"}}});
        assert_eq!(last_terminated_reason(&cs), "CrashLoopBackOff");
        let cs = json!({});
        assert_eq!(last_terminated_reason(&cs), "-");
    }
}
