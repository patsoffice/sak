//! `sak loki series <selector>` — list series (label sets) matching a
//! selector.
//!
//! Queries `/loki/api/v1/series?match[]=<selector>` and renders one series per
//! line in canonical form `{label="value",...}`, sorted ascending. Unlike
//! Prometheus, a Loki series object carries no `__name__` metric name (logs
//! have no metric), so each entry is just its label set — rendered by
//! [`crate::loki::query::format_labels`] verbatim.
//!
//! Optional `--start` / `--end` durations narrow the time window the discovery
//! walks; both are interpreted as "this many seconds ago" and map to
//! nanosecond-Unix-epoch `start` / `end` query parameters. With both omitted,
//! Loki applies its server-side default window.

use crate::output::Outcome;
use std::fmt::Write as _;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Result, anyhow};
use clap::Args;
use serde_json::Value;

use crate::duration::parse_duration;
use crate::loki::common_args::CommonLokiArgs;
use crate::loki::query::{format_labels, urlencode};
use crate::loki::runner::run_loki;

/// Nanoseconds per second — the multiplier from sak's second-granularity
/// duration windows to the nanosecond Unix epochs Loki's series API expects
/// (matches `crate::loki::range::NANOS_PER_SEC`).
const NANOS_PER_SEC: u64 = 1_000_000_000;

#[derive(Args)]
#[command(
    about = "List series matching a label selector",
    long_about = "List series from `/loki/api/v1/series` matching the given \
        LogQL stream selector (e.g. `{app=\"api\"}`, \
        `{job=\"varlogs\", level=~\"err.*\"}`). One series per line in \
        canonical form `{label=\"value\",...}`, sorted.\n\n\
        --start / --end take compact durations (s/m/h/d/w, chainable) and are \
        interpreted as time-since-now. With both omitted, Loki applies its \
        server-side default window.\n\n\
        Connection: pass --url <http://loki:3100> or set LOKI_URL.",
    after_help = "\
Examples:
  sak loki series '{app=\"api\"}'                       All streams for one app
  sak loki series '{namespace=\"prod\"}'                Streams in a namespace
  sak loki series '{app=\"api\"}' --start 1h            Active in the last hour
  sak loki series '{app=\"api\"}' --start 1d --end 1h   Discovered 1d-1h ago"
)]
pub struct SeriesArgs {
    #[command(flatten)]
    pub common: CommonLokiArgs,

    /// LogQL stream selector (e.g. `{app="api"}`)
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

    run_loki(&args.common, &path, |data| {
        let mut lines = extract_series_lines(data)?;
        lines.sort();
        Ok(lines)
    })
}

/// Build the `/loki/api/v1/series` request path. `start`/`end` are taken in
/// whole seconds and rendered as nanosecond Unix epochs (Loki's expected
/// unit). Pure so the parameter encoding is unit-testable without a clock or a
/// server.
///
/// `match[]` is percent-encoded as `match%5B%5D` rather than relying on Loki's
/// leniency about literal brackets in query strings.
fn build_series_path(selector: &str, start_secs: Option<u64>, end_secs: Option<u64>) -> String {
    let mut path = format!(
        "/loki/api/v1/series?{}={}",
        urlencode("match[]"),
        urlencode(selector)
    );
    if let Some(s) = start_secs {
        write!(path, "&start={}", s.saturating_mul(NANOS_PER_SEC)).unwrap();
    }
    if let Some(e) = end_secs {
        write!(path, "&end={}", e.saturating_mul(NANOS_PER_SEC)).unwrap();
    }
    path
}

/// Render the `/loki/api/v1/series` `data` array as `{labels}` lines. Each
/// entry is a bare label-set object (no metric name), so it goes straight
/// through [`format_labels`]. Pure so the formatting is unit-testable on
/// hand-built fixtures.
pub(super) fn extract_series_lines(data: &Value) -> Result<Vec<String>> {
    let arr = data
        .as_array()
        .ok_or_else(|| anyhow!("Loki /loki/api/v1/series `data` is not an array"))?;
    let mut out = Vec::with_capacity(arr.len());
    for series in arr {
        if !series.is_object() {
            return Err(anyhow!("series entry is not an object: {series:?}"));
        }
        out.push(format_labels(Some(series)));
    }
    Ok(out)
}

/// Current unix time in whole seconds. Surfaces a clear error rather than
/// panicking if the system clock is set before the unix epoch (mirrors
/// `crate::loki::range::unix_now`).
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
        let p = build_series_path("{app=\"api\"}", None, None);
        assert_eq!(p, "/loki/api/v1/series?match%5B%5D=%7Bapp%3D%22api%22%7D");
    }

    #[test]
    fn build_path_with_start_and_end_renders_ns() {
        let p = build_series_path("{app=\"api\"}", Some(100), Some(200));
        assert_eq!(
            p,
            "/loki/api/v1/series?match%5B%5D=%7Bapp%3D%22api%22%7D\
             &start=100000000000&end=200000000000"
        );
    }

    #[test]
    fn extract_series_lines_basic() {
        let data = json!([
            {"app": "api", "level": "info"},
            {"app": "web", "level": "error"}
        ]);
        let lines = extract_series_lines(&data).unwrap();
        assert_eq!(lines.len(), 2);
        assert!(lines.iter().any(|l| l == r#"{app="api",level="info"}"#));
        assert!(lines.iter().any(|l| l == r#"{app="web",level="error"}"#));
    }

    #[test]
    fn extract_series_lines_empty() {
        let lines = extract_series_lines(&json!([])).unwrap();
        assert!(lines.is_empty());
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
