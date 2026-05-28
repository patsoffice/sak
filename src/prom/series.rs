//! `sak prom series <selector>` — list series matching a label selector.
//!
//! Queries `/api/v1/series?match[]=<selector>` and renders one series per
//! line in canonical Prometheus form `metric{label="value",...}`, sorted
//! ascending. The metric name lives in the special `__name__` label and is
//! rendered outside the brace pair; every other label is escaped exactly
//! like [`crate::prom::query::format_labels`] (which can't be reused
//! verbatim because it does not split out `__name__`).
//!
//! Optional `--start` / `--end` durations narrow the time window the
//! discovery walks; both are interpreted as "this many seconds ago" and
//! map to unix-timestamp `start` / `end` query parameters. With both
//! omitted Prometheus applies its server-side default window.

use crate::output::Outcome;
use std::fmt::Write as _;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Result, anyhow};
use clap::Args;
use serde_json::Value;

use crate::prom::common_args::CommonPromArgs;
use crate::prom::duration::parse_duration;
use crate::prom::query::urlencode;
use crate::prom::runner::run_prom;

#[derive(Args)]
#[command(
    about = "List series matching a label selector",
    long_about = "List series from `/api/v1/series` matching the given \
        PromQL label selector (e.g. `up`, `{job=\"node\"}`, \
        `http_requests_total{code=~\"5..\"}`). One series per line in \
        canonical form `metric{label=\"value\",...}`, sorted.\n\n\
        --start / --end take compact durations (s/m/h/d/w, chainable) and \
        are interpreted as time-since-now. With both omitted, Prometheus \
        applies its server-side default window.\n\n\
        Connection: pass --url <http://prom:9090> or set PROMETHEUS_URL.",
    after_help = "\
Examples:
  sak prom series 'up'                                    All up{} series
  sak prom series '{job=\"node\"}'                         Series for one job
  sak prom series 'http_requests_total' --start 1h        Active in the last hour
  sak prom series 'up' --start 1d --end 1h                Discovered 1d-1h ago"
)]
pub struct SeriesArgs {
    #[command(flatten)]
    pub common: CommonPromArgs,

    /// PromQL label selector (e.g. `up`, `{job="node"}`)
    #[arg(value_name = "SELECTOR")]
    pub selector: String,

    /// Window start, as time-since-now (e.g. 1h, 30m, 2d)
    #[arg(long, value_name = "DURATION")]
    pub start: Option<String>,

    /// Window end, as time-since-now (e.g. 0s, 5m). Default: now.
    #[arg(long, value_name = "DURATION")]
    pub end: Option<String>,
}

pub fn run(args: &SeriesArgs) -> Result<Outcome> {
    let start_dur = args
        .start
        .as_deref()
        .map(|s| parse_duration(s).map_err(|e| anyhow!("--start: {e}")))
        .transpose()?;
    let end_dur = args
        .end
        .as_deref()
        .map(|s| parse_duration(s).map_err(|e| anyhow!("--end: {e}")))
        .transpose()?;

    let now = unix_now()?;
    let start_ts = start_dur.map(|d| now.saturating_sub(d));
    let end_ts = end_dur.map(|d| now.saturating_sub(d));

    let path = build_series_path(&args.selector, start_ts, end_ts);

    run_prom(&args.common, &path, |data| {
        let mut lines = extract_series_lines(data)?;
        lines.sort();
        Ok(lines)
    })
}

/// Build the `/api/v1/series` request path. Pure so the parameter encoding
/// is unit-testable without a clock or a server.
///
/// `match[]` is percent-encoded as `match%5B%5D` rather than relying on
/// Prometheus's leniency about literal brackets in query strings.
fn build_series_path(selector: &str, start: Option<u64>, end: Option<u64>) -> String {
    let mut path = format!(
        "/api/v1/series?{}={}",
        urlencode("match[]"),
        urlencode(selector)
    );
    if let Some(s) = start {
        write!(path, "&start={s}").unwrap();
    }
    if let Some(e) = end {
        write!(path, "&end={e}").unwrap();
    }
    path
}

/// Render the `/api/v1/series` `data` array as `metric{labels}` lines.
/// Pure so the formatting is unit-testable on hand-built fixtures.
pub(super) fn extract_series_lines(data: &Value) -> Result<Vec<String>> {
    let arr = data
        .as_array()
        .ok_or_else(|| anyhow!("Prometheus /api/v1/series `data` is not an array"))?;
    let mut out = Vec::with_capacity(arr.len());
    for series in arr {
        out.push(format_series(series)?);
    }
    Ok(out)
}

/// Format one series object as `metric{label="value",...}`. The `__name__`
/// label is moved out of the brace pair to act as the metric name; if it
/// is missing the metric renders as the empty string (the `{...}` form is
/// still valid PromQL).
fn format_series(series: &Value) -> Result<String> {
    let obj = series
        .as_object()
        .ok_or_else(|| anyhow!("series entry is not an object: {series:?}"))?;

    let metric_name = obj.get("__name__").and_then(Value::as_str).unwrap_or("");

    let mut entries: Vec<(&String, &Value)> =
        obj.iter().filter(|(k, _)| *k != "__name__").collect();
    entries.sort_by(|a, b| a.0.cmp(b.0));

    let mut out = String::from(metric_name);
    out.push('{');
    for (i, (k, v)) in entries.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        let raw = v.as_str().unwrap_or("");
        write!(out, "{}=\"{}\"", k, escape_label_value(raw)).unwrap();
    }
    out.push('}');
    Ok(out)
}

/// Escape the inside of a label value for Prometheus canonical form:
/// `\` → `\\`, `"` → `\"`, newline → `\n`. Matches the upstream serializer
/// (and `query::escape_label_value`, which is private to that module).
fn escape_label_value(s: &str) -> String {
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

/// Current unix time in whole seconds. Surfaces a clear error rather than
/// panicking if the system clock is set before the unix epoch (mirrors
/// `range::unix_now`).
fn unix_now() -> Result<u64> {
    Ok(SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| anyhow!("system clock is before the unix epoch: {e}"))?
        .as_secs())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn build_path_no_window() {
        let p = build_series_path("up", None, None);
        assert_eq!(p, "/api/v1/series?match%5B%5D=up");
    }

    #[test]
    fn build_path_with_start_and_end() {
        let p = build_series_path("up", Some(100), Some(200));
        assert_eq!(p, "/api/v1/series?match%5B%5D=up&start=100&end=200");
    }

    #[test]
    fn build_path_encodes_selector_braces() {
        let p = build_series_path("{job=\"node\"}", None, None);
        assert_eq!(p, "/api/v1/series?match%5B%5D=%7Bjob%3D%22node%22%7D");
    }

    #[test]
    fn format_series_with_name_and_labels() {
        let s = json!({"__name__": "up", "job": "node", "instance": "1.1.1.1:9100"});
        assert_eq!(
            format_series(&s).unwrap(),
            r#"up{instance="1.1.1.1:9100",job="node"}"#
        );
    }

    #[test]
    fn format_series_without_name_renders_empty_metric() {
        let s = json!({"job": "node"});
        assert_eq!(format_series(&s).unwrap(), r#"{job="node"}"#);
    }

    #[test]
    fn format_series_no_labels() {
        let s = json!({"__name__": "up"});
        assert_eq!(format_series(&s).unwrap(), "up{}");
    }

    #[test]
    fn format_series_escapes_special_chars_in_value() {
        let s = json!({"__name__": "x", "path": "a\"b\\c"});
        assert_eq!(format_series(&s).unwrap(), r#"x{path="a\"b\\c"}"#);
    }

    #[test]
    fn extract_series_lines_basic() {
        let data = json!([
            {"__name__": "up", "job": "a"},
            {"__name__": "up", "job": "b"}
        ]);
        let lines = extract_series_lines(&data).unwrap();
        assert_eq!(lines.len(), 2);
        assert!(lines.iter().any(|l| l == r#"up{job="a"}"#));
        assert!(lines.iter().any(|l| l == r#"up{job="b"}"#));
    }

    #[test]
    fn extract_series_lines_errors_on_non_array() {
        let err = extract_series_lines(&json!({})).unwrap_err();
        assert!(format!("{err}").contains("not an array"));
    }

    #[test]
    fn extract_series_lines_errors_on_non_object_element() {
        let err = extract_series_lines(&json!(["whoops"])).unwrap_err();
        assert!(format!("{err}").contains("not an object"));
    }
}
