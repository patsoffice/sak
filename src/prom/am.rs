//! `sak prom am {alerts | silences}` — Alertmanager v2 operations.
//!
//! Alertmanager's v2 API (`/api/v2/*`) returns JSON arrays directly, with
//! no Prometheus-style `{status, data, ...}` envelope, so these commands go
//! through [`PromClient::get_json`] rather than `get_prom`.
//!
//! Endpoints live behind their own env var (`ALERTMANAGER_URL`) and
//! `--url` flag so a single shell can target both Prometheus and
//! Alertmanager without re-exporting between commands. Default state
//! filter is `active` — what a triaging operator usually wants —
//! widened with `--all`.
//!
//! Two sub-subcommands share this file because Alertmanager's data model
//! is small and tightly cohesive (the silence matcher formatter is the
//! only non-trivial shared piece).

use std::io;
use std::process::ExitCode;

use anyhow::{Result, anyhow};
use clap::Args;
use regex::Regex;
use serde_json::Value;

use crate::output::BoundedWriter;
use crate::prom::client::{PromClient, resolve_endpoint};
use crate::prom::output::{collapse_newlines, emit_json};

// ---- `sak prom am alerts` ---------------------------------------------------

#[derive(Args)]
#[command(
    about = "List alerts on an Alertmanager server",
    long_about = "List alerts from Alertmanager's `/api/v2/alerts`. Default \
        is active alerts only; use --all to also include suppressed \
        (silenced) and unprocessed alerts. Use --name <regex> to filter by \
        alertname.\n\n\
        Output is one alert per line: \
        `state<TAB>alertname<TAB>severity<TAB>instance<TAB>startsAt<TAB>summary`, \
        sorted by (state, alertname, instance). The `state` is \
        Alertmanager's view (active / suppressed / unprocessed) — distinct \
        from Prometheus's (firing / pending / inactive) shown by \
        `sak prom alerts`.\n\n\
        Connection: pass --url <http://am:9093> or set ALERTMANAGER_URL.",
    after_help = "\
Examples:
  sak prom am alerts                              Active alerts only
  sak prom am alerts --all                        Include suppressed/unprocessed
  sak prom am alerts --name 'Cert.*'              Filter by alertname regex
  sak prom am alerts --url http://am:9093 --json  Raw JSON for piping"
)]
pub struct AmAlertsArgs {
    /// Alertmanager base URL (overrides ALERTMANAGER_URL env)
    #[arg(long, value_name = "URL")]
    pub url: Option<String>,

    /// Include suppressed (silenced) and unprocessed alerts
    #[arg(long)]
    pub all: bool,

    /// Filter by alertname regex
    #[arg(long, value_name = "REGEX")]
    pub name: Option<String>,

    /// Emit the raw JSON response from /api/v2/alerts
    #[arg(long)]
    pub json: bool,

    /// Maximum number of output lines
    #[arg(long)]
    pub limit: Option<usize>,
}

/// One row extracted from an Alertmanager alert. Pure data so it can be
/// unit-tested on hand-built fixtures with no live server.
pub(super) struct AmAlertRow {
    pub state: String,
    pub alertname: String,
    pub severity: String,
    pub instance: String,
    pub starts_at: String,
    pub summary: String,
}

pub(super) fn extract_am_alert_row(alert: &Value) -> AmAlertRow {
    let labels = alert.get("labels");
    let annotations = alert.get("annotations");
    AmAlertRow {
        state: alert
            .get("status")
            .and_then(|s| s.get("state"))
            .and_then(Value::as_str)
            .unwrap_or("-")
            .to_string(),
        alertname: label(labels, "alertname").unwrap_or("-").to_string(),
        severity: label(labels, "severity").unwrap_or("-").to_string(),
        instance: label(labels, "instance").unwrap_or("-").to_string(),
        starts_at: alert
            .get("startsAt")
            .and_then(Value::as_str)
            .unwrap_or("-")
            .to_string(),
        summary: annotations
            .and_then(|a| a.get("summary"))
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
    }
}

fn label<'a>(labels: Option<&'a Value>, key: &str) -> Option<&'a str> {
    labels.and_then(|l| l.get(key)).and_then(Value::as_str)
}

pub(super) fn format_am_alert_row(row: &AmAlertRow) -> String {
    format!(
        "{}\t{}\t{}\t{}\t{}\t{}",
        row.state,
        row.alertname,
        row.severity,
        row.instance,
        row.starts_at,
        collapse_newlines(&row.summary)
    )
}

pub(super) fn sort_alert_rows(rows: &mut [AmAlertRow]) {
    rows.sort_by(|a, b| {
        a.state
            .cmp(&b.state)
            .then_with(|| a.alertname.cmp(&b.alertname))
            .then_with(|| a.instance.cmp(&b.instance))
    });
}

pub fn alerts(args: &AmAlertsArgs) -> Result<ExitCode> {
    let endpoint = resolve_endpoint(args.url.as_deref(), "ALERTMANAGER_URL")?;
    let client = PromClient::new(endpoint);
    let data = match client.get_json("/api/v2/alerts")? {
        Some(v) => v,
        None => return Ok(ExitCode::from(1)),
    };

    if args.json {
        return emit_json(&data, args.limit);
    }

    let alerts = data
        .as_array()
        .ok_or_else(|| anyhow!("Alertmanager /api/v2/alerts response is not an array"))?;

    let name_re = match &args.name {
        Some(s) => Some(Regex::new(s).map_err(|e| anyhow!("invalid --name regex: {e}"))?),
        None => None,
    };

    let mut rows: Vec<AmAlertRow> = alerts
        .iter()
        .map(extract_am_alert_row)
        .filter(|r| args.all || r.state == "active")
        .filter(|r| name_re.as_ref().is_none_or(|re| re.is_match(&r.alertname)))
        .collect();
    sort_alert_rows(&mut rows);

    write_rows(&rows, format_am_alert_row, args.limit)
}

// ---- `sak prom am silences` -------------------------------------------------

#[derive(Args)]
#[command(
    about = "List silences on an Alertmanager server",
    long_about = "List silences from Alertmanager's `/api/v2/silences`. \
        Default is active silences only; use --all to also include expired \
        and pending. Matchers are rendered in Prometheus PromQL form: \
        `key=\"v\"` for exact, `key=~\"v\"` for regex, plus `!=` / `!~` \
        for negated variants — so the output can be pasted directly into \
        a PromQL filter.\n\n\
        Output is one silence per line: \
        `state<TAB>id<TAB>endsAt<TAB>createdBy<TAB>matchers<TAB>comment`, \
        sorted by (state, id).\n\n\
        Connection: pass --url <http://am:9093> or set ALERTMANAGER_URL.",
    after_help = "\
Examples:
  sak prom am silences                            Active silences only
  sak prom am silences --all                      Include expired + pending
  sak prom am silences --json                     Raw JSON for piping"
)]
pub struct AmSilencesArgs {
    /// Alertmanager base URL (overrides ALERTMANAGER_URL env)
    #[arg(long, value_name = "URL")]
    pub url: Option<String>,

    /// Include expired and pending silences
    #[arg(long)]
    pub all: bool,

    /// Emit the raw JSON response from /api/v2/silences
    #[arg(long)]
    pub json: bool,

    /// Maximum number of output lines
    #[arg(long)]
    pub limit: Option<usize>,
}

/// One row extracted from an Alertmanager silence.
pub(super) struct AmSilenceRow {
    pub state: String,
    pub id: String,
    pub ends_at: String,
    pub created_by: String,
    pub matchers: String,
    pub comment: String,
}

pub(super) fn extract_am_silence_row(silence: &Value) -> AmSilenceRow {
    AmSilenceRow {
        state: silence
            .get("status")
            .and_then(|s| s.get("state"))
            .and_then(Value::as_str)
            .unwrap_or("-")
            .to_string(),
        id: str_or(silence.get("id"), "-"),
        ends_at: str_or(silence.get("endsAt"), "-"),
        created_by: str_or(silence.get("createdBy"), "-"),
        matchers: format_matchers(silence.get("matchers")),
        comment: str_or(silence.get("comment"), ""),
    }
}

fn str_or(v: Option<&Value>, default: &str) -> String {
    v.and_then(Value::as_str).unwrap_or(default).to_string()
}

pub(super) fn format_am_silence_row(row: &AmSilenceRow) -> String {
    format!(
        "{}\t{}\t{}\t{}\t{}\t{}",
        row.state,
        row.id,
        row.ends_at,
        row.created_by,
        row.matchers,
        collapse_newlines(&row.comment)
    )
}

pub(super) fn sort_silence_rows(rows: &mut [AmSilenceRow]) {
    rows.sort_by(|a, b| a.state.cmp(&b.state).then_with(|| a.id.cmp(&b.id)))
}

/// Render an Alertmanager silence `matchers` array as a comma-separated
/// PromQL-style matcher string. Operator picked from the `isEqual` and
/// `isRegex` booleans:
///
/// | isEqual | isRegex | op   |
/// |---------|---------|------|
/// | true    | false   | `=`  |
/// | true    | true    | `=~` |
/// | false   | false   | `!=` |
/// | false   | true    | `!~` |
///
/// `isEqual` defaults to `true` when absent — older Alertmanager releases
/// omit the field, and the v2 schema documents `true` as the default.
pub(super) fn format_matchers(matchers: Option<&Value>) -> String {
    let arr = match matchers.and_then(Value::as_array) {
        Some(a) => a,
        None => return String::new(),
    };
    let mut parts = Vec::with_capacity(arr.len());
    for m in arr {
        let name = m.get("name").and_then(Value::as_str).unwrap_or("");
        let value = m.get("value").and_then(Value::as_str).unwrap_or("");
        let is_regex = m.get("isRegex").and_then(Value::as_bool).unwrap_or(false);
        let is_equal = m.get("isEqual").and_then(Value::as_bool).unwrap_or(true);
        let op = match (is_equal, is_regex) {
            (true, false) => "=",
            (true, true) => "=~",
            (false, false) => "!=",
            (false, true) => "!~",
        };
        parts.push(format!("{name}{op}\"{}\"", escape_matcher_value(value)));
    }
    parts.join(",")
}

/// Escape a matcher value: `\` -> `\\`, `"` -> `\"`, newline -> `\n`.
/// Mirrors the upstream Prometheus serialization so the rendered matcher
/// string can be pasted into a PromQL selector and re-parsed.
fn escape_matcher_value(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            other => out.push(other),
        }
    }
    out
}

pub fn silences(args: &AmSilencesArgs) -> Result<ExitCode> {
    let endpoint = resolve_endpoint(args.url.as_deref(), "ALERTMANAGER_URL")?;
    let client = PromClient::new(endpoint);
    let data = match client.get_json("/api/v2/silences")? {
        Some(v) => v,
        None => return Ok(ExitCode::from(1)),
    };

    if args.json {
        return emit_json(&data, args.limit);
    }

    let silences = data
        .as_array()
        .ok_or_else(|| anyhow!("Alertmanager /api/v2/silences response is not an array"))?;

    let mut rows: Vec<AmSilenceRow> = silences
        .iter()
        .map(extract_am_silence_row)
        .filter(|r| args.all || r.state == "active")
        .collect();
    sort_silence_rows(&mut rows);

    write_rows(&rows, format_am_silence_row, args.limit)
}

// ---- shared write loop ------------------------------------------------------

/// Drive a `BoundedWriter` over a slice of rows, mapping the row → line
/// closure once per row. Returns sak's standard exit 0 (wrote anything) /
/// exit 1 (empty) split. Generic so the same body serves both the alert
/// row and silence row paths.
fn write_rows<T, F>(rows: &[T], format: F, limit: Option<usize>) -> Result<ExitCode>
where
    F: Fn(&T) -> String,
{
    let stdout = io::stdout();
    let handle = stdout.lock();
    let mut writer = BoundedWriter::new(handle, limit);

    let mut wrote_any = false;
    for row in rows {
        if !writer.write_line(&format(row))? {
            break;
        }
        wrote_any = true;
    }
    writer.flush()?;
    Ok(if wrote_any {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // ---- alert extraction / format / sort ----

    #[test]
    fn extract_alert_row_basic() {
        let alert = json!({
            "labels": {"alertname": "Watchdog", "severity": "none", "instance": "h:9093"},
            "annotations": {"summary": "test"},
            "status": {"state": "active", "silencedBy": [], "inhibitedBy": []},
            "startsAt": "2026-05-14T12:00:00Z",
            "endsAt": "0001-01-01T00:00:00Z"
        });
        let row = extract_am_alert_row(&alert);
        assert_eq!(row.state, "active");
        assert_eq!(row.alertname, "Watchdog");
        assert_eq!(row.severity, "none");
        assert_eq!(row.instance, "h:9093");
        assert_eq!(row.starts_at, "2026-05-14T12:00:00Z");
        assert_eq!(row.summary, "test");
    }

    #[test]
    fn extract_alert_row_missing_fields_use_dashes() {
        let row = extract_am_alert_row(&json!({}));
        assert_eq!(row.state, "-");
        assert_eq!(row.alertname, "-");
        assert_eq!(row.severity, "-");
        assert_eq!(row.instance, "-");
        assert_eq!(row.starts_at, "-");
        assert_eq!(row.summary, "");
    }

    #[test]
    fn extract_alert_row_suppressed_state() {
        let alert = json!({
            "labels": {"alertname": "X"},
            "status": {"state": "suppressed"}
        });
        let row = extract_am_alert_row(&alert);
        assert_eq!(row.state, "suppressed");
    }

    #[test]
    fn format_alert_row_collapses_summary_newlines() {
        let row = AmAlertRow {
            state: "active".into(),
            alertname: "X".into(),
            severity: "-".into(),
            instance: "-".into(),
            starts_at: "-".into(),
            summary: "line1\nline2".into(),
        };
        let line = format_am_alert_row(&row);
        assert!(line.contains("line1 line2"));
    }

    #[test]
    fn sort_alert_rows_by_state_then_name_then_instance() {
        let mut rows = vec![
            alert_row("active", "B", "i1"),
            alert_row("active", "A", "i2"),
            alert_row("active", "A", "i1"),
            alert_row("suppressed", "Z", "i0"),
        ];
        sort_alert_rows(&mut rows);
        assert_eq!(
            rows.iter()
                .map(|r| (r.state.as_str(), r.alertname.as_str(), r.instance.as_str()))
                .collect::<Vec<_>>(),
            vec![
                ("active", "A", "i1"),
                ("active", "A", "i2"),
                ("active", "B", "i1"),
                ("suppressed", "Z", "i0"),
            ]
        );
    }

    fn alert_row(state: &str, alertname: &str, instance: &str) -> AmAlertRow {
        AmAlertRow {
            state: state.into(),
            alertname: alertname.into(),
            severity: "-".into(),
            instance: instance.into(),
            starts_at: "-".into(),
            summary: String::new(),
        }
    }

    // ---- silence extraction / format / sort ----

    #[test]
    fn extract_silence_row_basic() {
        let silence = json!({
            "id": "abc-123",
            "createdBy": "ops",
            "comment": "deploy",
            "startsAt": "2026-05-14T11:00:00Z",
            "endsAt": "2026-05-14T13:00:00Z",
            "matchers": [
                {"name": "alertname", "value": "X", "isRegex": false, "isEqual": true}
            ],
            "status": {"state": "active"}
        });
        let row = extract_am_silence_row(&silence);
        assert_eq!(row.state, "active");
        assert_eq!(row.id, "abc-123");
        assert_eq!(row.ends_at, "2026-05-14T13:00:00Z");
        assert_eq!(row.created_by, "ops");
        assert_eq!(row.matchers, r#"alertname="X""#);
        assert_eq!(row.comment, "deploy");
    }

    #[test]
    fn extract_silence_row_missing_fields_use_dashes() {
        let row = extract_am_silence_row(&json!({}));
        assert_eq!(row.state, "-");
        assert_eq!(row.id, "-");
        assert_eq!(row.ends_at, "-");
        assert_eq!(row.created_by, "-");
        assert_eq!(row.matchers, "");
        assert_eq!(row.comment, "");
    }

    #[test]
    fn format_silence_row_collapses_comment_newlines() {
        let row = AmSilenceRow {
            state: "active".into(),
            id: "x".into(),
            ends_at: "-".into(),
            created_by: "-".into(),
            matchers: String::new(),
            comment: "first\nsecond".into(),
        };
        assert!(format_am_silence_row(&row).contains("first second"));
    }

    #[test]
    fn sort_silence_rows_by_state_then_id() {
        let mut rows = vec![
            silence_row("active", "b"),
            silence_row("expired", "a"),
            silence_row("active", "a"),
        ];
        sort_silence_rows(&mut rows);
        assert_eq!(
            rows.iter()
                .map(|r| (r.state.as_str(), r.id.as_str()))
                .collect::<Vec<_>>(),
            vec![("active", "a"), ("active", "b"), ("expired", "a")]
        );
    }

    fn silence_row(state: &str, id: &str) -> AmSilenceRow {
        AmSilenceRow {
            state: state.into(),
            id: id.into(),
            ends_at: "-".into(),
            created_by: "-".into(),
            matchers: String::new(),
            comment: String::new(),
        }
    }

    // ---- matcher formatting ----

    #[test]
    fn format_matchers_empty_or_missing() {
        assert_eq!(format_matchers(None), "");
        assert_eq!(format_matchers(Some(&json!([]))), "");
    }

    #[test]
    fn format_matchers_all_four_operators() {
        let m = json!([
            {"name": "a", "value": "1", "isRegex": false, "isEqual": true},
            {"name": "b", "value": "2", "isRegex": true,  "isEqual": true},
            {"name": "c", "value": "3", "isRegex": false, "isEqual": false},
            {"name": "d", "value": "4", "isRegex": true,  "isEqual": false}
        ]);
        assert_eq!(format_matchers(Some(&m)), r#"a="1",b=~"2",c!="3",d!~"4""#);
    }

    /// Older Alertmanager versions don't emit `isEqual`; v2 documents the
    /// default as `true`, so a matcher with `isRegex` only should still
    /// render as `=` / `=~` (positive), not `!=` / `!~`.
    #[test]
    fn format_matchers_defaults_is_equal_to_true_when_absent() {
        let m = json!([
            {"name": "a", "value": "1", "isRegex": false},
            {"name": "b", "value": "2", "isRegex": true}
        ]);
        assert_eq!(format_matchers(Some(&m)), r#"a="1",b=~"2""#);
    }

    #[test]
    fn format_matchers_escapes_value_special_chars() {
        let m = json!([
            {"name": "path", "value": "a\"b\\c\nd", "isRegex": false, "isEqual": true}
        ]);
        assert_eq!(format_matchers(Some(&m)), r#"path="a\"b\\c\nd""#);
    }
}
