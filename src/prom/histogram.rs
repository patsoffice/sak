//! `sak prom histogram <metric> [--labels k=v,...]` — pretty-print a
//! Prometheus histogram's `_bucket` series.
//!
//! Histogram buckets are the single most opaque corner of the Prometheus
//! data model: the `_bucket` series are *cumulative* counters keyed by an
//! `le` ("less than or equal to") label, so reading them raw means doing
//! subtraction in your head. This command does it for you — for each `le`
//! bucket it shows the cumulative count, the per-bucket delta, and the 5m
//! rate side by side.
//!
//! Two instant queries are issued and merged by `le`:
//! - `sum by (le) (<metric>_bucket{<labels>})` — cumulative counts
//! - `sum by (le) (rate(<metric>_bucket{<labels>}[<rate-window>]))` — rate
//!
//! The `le` label is rendered through a unit lens (`--unit`, or auto-detected
//! from the metric name): a `_seconds` histogram renders `le` as a duration
//! (`<30.00d`), a `_bytes` histogram as a size (`<1.00MiB`), anything else
//! verbatim. This is what turns the cert-expiry triage histogram from
//! `le="2.592e+06"` into `le=<30.00d`.

use std::cmp::Ordering;
use std::collections::HashMap;
use std::io;
use std::process::ExitCode;

use anyhow::{Result, anyhow, bail};
use clap::{Args, ValueEnum};
use serde_json::Value;

use crate::output::BoundedWriter;
use crate::prom::client::{PromClient, resolve_endpoint};
use crate::prom::common_args::CommonPromArgs;
use crate::prom::duration::parse_duration;
use crate::prom::output::emit_json;
use crate::prom::query::urlencode;

#[derive(Args)]
#[command(
    about = "Pretty-print a Prometheus histogram's buckets",
    long_about = "Render a Prometheus histogram's `_bucket` series as one row \
        per `le` bucket: cumulative count, per-bucket delta, and rate. The \
        metric name may be given with or without the `_bucket` suffix.\n\n\
        Issues two instant queries — `sum by (le) (<metric>_bucket)` for the \
        cumulative counts and `sum by (le) (rate(<metric>_bucket[<window>]))` \
        for the rate — and merges them by `le`.\n\n\
        The `le` label is rendered via --unit: `duration` treats it as \
        seconds (<30.00d), `bytes` as a size (<1.00MiB), `raw` verbatim. \
        When --unit is omitted it is auto-detected from the metric name \
        (`_seconds` -> duration, `_bytes` -> bytes, else raw).\n\n\
        Connection: pass --url <http://prom:9090> or set PROMETHEUS_URL.",
    after_help = "\
Examples:
  sak prom histogram apiserver_request_duration_seconds
  sak prom histogram apiserver_request_duration_seconds --labels verb=GET
  sak prom histogram prometheus_http_response_size_bytes --unit bytes
  sak prom histogram some_metric_bucket --rate-window 1m --json"
)]
pub struct HistogramArgs {
    #[command(flatten)]
    pub common: CommonPromArgs,

    /// Histogram metric name, with or without the `_bucket` suffix
    #[arg(value_name = "METRIC")]
    pub metric: String,

    /// Exact-match label filters, comma-separated (e.g. `verb=GET,code=200`)
    #[arg(long, value_name = "K=V,...")]
    pub labels: Option<String>,

    /// Window for the rate() column (e.g. 1m, 5m, 1h)
    #[arg(long, value_name = "DURATION", default_value = "5m")]
    pub rate_window: String,

    /// How to render the `le` label (default: auto-detect from metric name)
    #[arg(long, value_enum, value_name = "UNIT")]
    pub unit: Option<LeUnit>,
}

/// How to interpret and render the numeric `le` bucket bound.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum LeUnit {
    /// Print the `le` value verbatim
    Raw,
    /// Interpret `le` as seconds; render as a duration (`<30.00d`)
    Duration,
    /// Interpret `le` as bytes; render as a size (`<1.00MiB`)
    Bytes,
}

/// One rendered histogram bucket. Pure data so the merge + delta logic is
/// unit-testable on hand-built fixtures with no live server.
pub(super) struct BucketRow {
    pub le_str: String,
    pub le_val: f64,
    pub cum: f64,
    pub delta: f64,
    pub rate: Option<f64>,
}

pub fn run(args: &HistogramArgs) -> Result<ExitCode> {
    let rate_window =
        parse_duration(&args.rate_window).map_err(|e| anyhow!("--rate-window: {e}"))?;
    if rate_window == 0 {
        return Err(anyhow!("--rate-window must be a non-zero duration"));
    }

    let bucket_metric = bucket_metric_name(&args.metric);
    let base_metric = bucket_metric
        .strip_suffix("_bucket")
        .unwrap_or(&bucket_metric);
    let unit = args.unit.unwrap_or_else(|| detect_unit(base_metric));

    let labels = parse_labels(args.labels.as_deref())?;
    let matchers = build_matcher(&labels);
    let cum_query = build_cumulative_query(&bucket_metric, &matchers);
    let rate_query = build_rate_query(&bucket_metric, &matchers, rate_window);

    let endpoint = resolve_endpoint(args.common.url.as_deref(), "PROMETHEUS_URL")?;
    let client = PromClient::new(endpoint);

    let cum_data =
        match client.get_prom(&format!("/api/v1/query?query={}", urlencode(&cum_query)))? {
            Some(v) => v,
            None => return Ok(ExitCode::from(1)),
        };
    let rate_data =
        match client.get_prom(&format!("/api/v1/query?query={}", urlencode(&rate_query)))? {
            Some(v) => v,
            None => return Ok(ExitCode::from(1)),
        };

    if args.common.json {
        let combined = serde_json::json!({
            "cumulative": cum_data,
            "rate": rate_data,
        });
        return emit_json(&combined, args.common.limit);
    }

    let rows = build_buckets(&cum_data, &rate_data)?;
    if rows.is_empty() {
        return Ok(ExitCode::from(1));
    }

    let stdout = io::stdout();
    let handle = stdout.lock();
    let mut writer = BoundedWriter::new(handle, args.common.limit);

    let mut wrote_any = false;
    for row in &rows {
        if !writer.write_line(&format_bucket_row(row, unit))? {
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

/// Append `_bucket` unless the caller already did. Prometheus histogram
/// series are always `<name>_bucket`, but users think in terms of the
/// histogram's base name, so accept either.
fn bucket_metric_name(metric: &str) -> String {
    if metric.ends_with("_bucket") {
        metric.to_string()
    } else {
        format!("{metric}_bucket")
    }
}

/// Auto-detect the `le` unit from the (bucket-suffix-stripped) metric name.
/// Prometheus naming conventions mandate base units, so a `_seconds` suffix
/// reliably means `le` is in seconds and `_bytes` means bytes.
fn detect_unit(base_metric: &str) -> LeUnit {
    if base_metric.ends_with("_seconds") {
        LeUnit::Duration
    } else if base_metric.ends_with("_bytes") {
        LeUnit::Bytes
    } else {
        LeUnit::Raw
    }
}

/// Parse the `--labels k=v,k2=v2` argument into key/value pairs. An empty
/// or absent argument yields no filters. Splits each entry on the first
/// `=` so values may themselves contain `=`.
fn parse_labels(labels: Option<&str>) -> Result<Vec<(String, String)>> {
    let raw = match labels {
        Some(s) if !s.is_empty() => s,
        _ => return Ok(Vec::new()),
    };
    let mut out = Vec::new();
    for part in raw.split(',') {
        let (k, v) = part
            .split_once('=')
            .ok_or_else(|| anyhow!("--labels entry {part:?} is not in k=v form"))?;
        if k.is_empty() {
            return Err(anyhow!("--labels entry {part:?} has an empty key"));
        }
        out.push((k.to_string(), v.to_string()));
    }
    Ok(out)
}

/// Build a PromQL label-matcher suffix `{k="v",...}` for exact matches.
/// Returns an empty string when there are no labels so the query is just
/// `<metric>_bucket`. Values are escaped so a `"` in a value can't break
/// out of the matcher.
fn build_matcher(labels: &[(String, String)]) -> String {
    if labels.is_empty() {
        return String::new();
    }
    let mut out = String::from("{");
    for (i, (k, v)) in labels.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push_str(k);
        out.push_str("=\"");
        for c in v.chars() {
            match c {
                '\\' => out.push_str("\\\\"),
                '"' => out.push_str("\\\""),
                other => out.push(other),
            }
        }
        out.push('"');
    }
    out.push('}');
    out
}

/// PromQL for the cumulative bucket counts: `sum by (le) (<bucket>{<matchers>})`.
/// Pure so the query string is unit-testable without a server.
fn build_cumulative_query(bucket_metric: &str, matchers: &str) -> String {
    format!("sum by (le) ({bucket_metric}{matchers})")
}

/// PromQL for the per-second bucket rate over a `rate_window`-second window:
/// `sum by (le) (rate(<bucket>{<matchers>}[<n>s]))`. Pure so the query string
/// is unit-testable without a server.
fn build_rate_query(bucket_metric: &str, matchers: &str, rate_window: u64) -> String {
    format!("sum by (le) (rate({bucket_metric}{matchers}[{rate_window}s]))")
}

/// Merge the cumulative-count and rate vector responses into `le`-sorted
/// bucket rows with per-bucket deltas. Pure — unit-tested on fixtures.
///
/// Buckets are ordered by numeric `le` value, so `+Inf` (which parses to
/// `f64::INFINITY`) sorts last and the delta walk is well-defined. A bucket
/// present in the cumulative result but absent from the rate result gets
/// `rate: None`, rendered as `-`.
pub(super) fn build_buckets(cum_data: &Value, rate_data: &Value) -> Result<Vec<BucketRow>> {
    let cum_map = vector_by_le(cum_data).map_err(|e| anyhow!("cumulative query: {e}"))?;
    let rate_map = vector_by_le(rate_data).map_err(|e| anyhow!("rate query: {e}"))?;

    let mut les: Vec<(String, f64)> = cum_map
        .iter()
        .map(|(k, (le_val, _))| (k.clone(), *le_val))
        .collect();
    les.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(Ordering::Equal));

    let mut rows = Vec::with_capacity(les.len());
    let mut prev_cum = 0.0_f64;
    for (le_str, le_val) in les {
        let cum = cum_map[&le_str].1;
        let delta = cum - prev_cum;
        prev_cum = cum;
        let rate = rate_map.get(&le_str).map(|(_, r)| *r);
        rows.push(BucketRow {
            le_str,
            le_val,
            cum,
            delta,
            rate,
        });
    }
    Ok(rows)
}

/// Index a `sum by (le) (...)` vector response by its `le` label, mapping
/// each to `(le as f64, value as f64)`. Errors if the response isn't a
/// vector or a series is missing/!numeric `le` or value — better to fail
/// loudly than silently drop buckets.
fn vector_by_le(data: &Value) -> Result<HashMap<String, (f64, f64)>> {
    let result_type = data
        .get("resultType")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("response has no `resultType`"))?;
    if result_type != "vector" {
        bail!("expected a vector result, got {result_type:?}");
    }
    let series = data
        .get("result")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("response `result` is not an array"))?;

    let mut map = HashMap::with_capacity(series.len());
    for s in series {
        let le_str = s
            .get("metric")
            .and_then(|m| m.get("le"))
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("series is missing the `le` label"))?
            .to_string();
        // `"+Inf"` parses to f64::INFINITY; finite buckets parse normally.
        let le_val = le_str
            .parse::<f64>()
            .map_err(|_| anyhow!("`le` label {le_str:?} is not numeric"))?;
        let val_str = s
            .get("value")
            .and_then(Value::as_array)
            .and_then(|a| a.get(1))
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("series value is not [<ts>, \"<value>\"]"))?;
        let val = val_str
            .parse::<f64>()
            .map_err(|_| anyhow!("series value {val_str:?} is not numeric"))?;
        map.insert(le_str, (le_val, val));
    }
    Ok(map)
}

/// Format one bucket row as the tab-separated, self-describing line
/// `le=<...>\tcum=<n>\tdelta=<n>\trate=<n>/s`. The `key=` prefixes follow
/// the `sak sqlite info` precedent — histogram columns aren't obvious
/// positionally, so each cell names itself.
pub(super) fn format_bucket_row(row: &BucketRow, unit: LeUnit) -> String {
    let le = format_le(&row.le_str, row.le_val, unit);
    let rate = match row.rate {
        Some(r) => format!("{r:.4}/s"),
        None => "-".to_string(),
    };
    format!(
        "le={}\tcum={}\tdelta={}\trate={}",
        le,
        format_count(row.cum),
        format_count(row.delta),
        rate
    )
}

/// Render the `le` bound through the chosen unit lens. `+Inf` is always
/// passed through verbatim; every finite bucket gets a `<` prefix because
/// `le` means "less than or equal to".
fn format_le(le_str: &str, le_val: f64, unit: LeUnit) -> String {
    if le_val.is_infinite() {
        return "+Inf".to_string();
    }
    match unit {
        LeUnit::Raw => format!("<{le_str}"),
        LeUnit::Duration => format!("<{}", format_seconds(le_val)),
        LeUnit::Bytes => format!("<{}", format_bytes(le_val)),
    }
}

/// Format a seconds value as the largest unit that keeps the number >= 1
/// (`86_400.0` -> `1.00d`, `0.5` -> `500.00ms`).
fn format_seconds(secs: f64) -> String {
    let (val, unit) = if secs >= 86_400.0 {
        (secs / 86_400.0, "d")
    } else if secs >= 3_600.0 {
        (secs / 3_600.0, "h")
    } else if secs >= 60.0 {
        (secs / 60.0, "m")
    } else if secs >= 1.0 {
        (secs, "s")
    } else if secs > 0.0 {
        (secs * 1_000.0, "ms")
    } else {
        (0.0, "s")
    };
    format!("{val:.2}{unit}")
}

/// Format a bytes value with binary (1024) prefixes (`1_048_576.0` ->
/// `1.00MiB`).
fn format_bytes(bytes: f64) -> String {
    const UNITS: &[&str] = &["B", "KiB", "MiB", "GiB", "TiB", "PiB"];
    let mut val = bytes;
    let mut idx = 0;
    while val >= 1_024.0 && idx < UNITS.len() - 1 {
        val /= 1_024.0;
        idx += 1;
    }
    format!("{val:.2}{}", UNITS[idx])
}

/// Render a count: whole numbers print without a decimal point, fractional
/// values (and very large magnitudes where `{:.0}` would be misleading)
/// fall back to the default float formatting.
fn format_count(v: f64) -> String {
    if v.is_finite() && v.fract() == 0.0 && v.abs() < 1e15 {
        format!("{v:.0}")
    } else {
        format!("{v}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn bucket_metric_name_appends_suffix() {
        assert_eq!(bucket_metric_name("foo_seconds"), "foo_seconds_bucket");
    }

    #[test]
    fn bucket_metric_name_leaves_existing_suffix() {
        assert_eq!(
            bucket_metric_name("foo_seconds_bucket"),
            "foo_seconds_bucket"
        );
    }

    #[test]
    fn detect_unit_from_metric_name() {
        assert_eq!(
            detect_unit("apiserver_request_duration_seconds"),
            LeUnit::Duration
        );
        assert_eq!(
            detect_unit("prometheus_http_response_size_bytes"),
            LeUnit::Bytes
        );
        assert_eq!(detect_unit("some_random_metric"), LeUnit::Raw);
    }

    #[test]
    fn parse_labels_basic() {
        let got = parse_labels(Some("verb=GET,code=200")).unwrap();
        assert_eq!(
            got,
            vec![
                ("verb".to_string(), "GET".to_string()),
                ("code".to_string(), "200".to_string()),
            ]
        );
    }

    #[test]
    fn parse_labels_empty_or_absent() {
        assert!(parse_labels(None).unwrap().is_empty());
        assert!(parse_labels(Some("")).unwrap().is_empty());
    }

    #[test]
    fn parse_labels_value_may_contain_equals() {
        let got = parse_labels(Some("query=a=b")).unwrap();
        assert_eq!(got, vec![("query".to_string(), "a=b".to_string())]);
    }

    #[test]
    fn parse_labels_rejects_malformed() {
        assert!(parse_labels(Some("noequals")).is_err());
        assert!(parse_labels(Some("=value")).is_err());
    }

    #[test]
    fn build_matcher_empty() {
        assert_eq!(build_matcher(&[]), "");
    }

    #[test]
    fn build_matcher_single_and_multiple() {
        assert_eq!(
            build_matcher(&[("job".to_string(), "node".to_string())]),
            r#"{job="node"}"#
        );
        assert_eq!(
            build_matcher(&[
                ("job".to_string(), "node".to_string()),
                ("inst".to_string(), "a:1".to_string()),
            ]),
            r#"{job="node",inst="a:1"}"#
        );
    }

    #[test]
    fn build_matcher_escapes_quotes_and_backslashes() {
        assert_eq!(
            build_matcher(&[("path".to_string(), "a\"b\\c".to_string())]),
            r#"{path="a\"b\\c"}"#
        );
    }

    #[test]
    fn build_cumulative_query_wraps_in_sum_by_le() {
        assert_eq!(
            build_cumulative_query("http_request_duration_seconds_bucket", ""),
            "sum by (le) (http_request_duration_seconds_bucket)"
        );
        assert_eq!(
            build_cumulative_query("http_request_duration_seconds_bucket", r#"{job="api"}"#),
            r#"sum by (le) (http_request_duration_seconds_bucket{job="api"})"#
        );
    }

    #[test]
    fn build_rate_query_wraps_in_rate_with_window() {
        assert_eq!(
            build_rate_query("http_request_duration_seconds_bucket", "", 300),
            "sum by (le) (rate(http_request_duration_seconds_bucket[300s]))"
        );
        assert_eq!(
            build_rate_query("http_request_duration_seconds_bucket", r#"{job="api"}"#, 60),
            r#"sum by (le) (rate(http_request_duration_seconds_bucket{job="api"}[60s]))"#
        );
    }

    #[test]
    fn format_seconds_picks_largest_unit() {
        assert_eq!(format_seconds(86_400.0), "1.00d");
        assert_eq!(format_seconds(2_592_000.0), "30.00d");
        assert_eq!(format_seconds(3_600.0), "1.00h");
        assert_eq!(format_seconds(90.0), "1.50m");
        assert_eq!(format_seconds(5.0), "5.00s");
        assert_eq!(format_seconds(0.5), "500.00ms");
        assert_eq!(format_seconds(0.0), "0.00s");
    }

    #[test]
    fn format_bytes_uses_binary_prefixes() {
        assert_eq!(format_bytes(512.0), "512.00B");
        assert_eq!(format_bytes(1_024.0), "1.00KiB");
        assert_eq!(format_bytes(1_048_576.0), "1.00MiB");
        assert_eq!(format_bytes(1_073_741_824.0), "1.00GiB");
    }

    #[test]
    fn format_le_variants() {
        assert_eq!(
            format_le("2.592e+06", 2_592_000.0, LeUnit::Duration),
            "<30.00d"
        );
        assert_eq!(format_le("1024", 1_024.0, LeUnit::Bytes), "<1.00KiB");
        assert_eq!(
            format_le("2.592e+06", 2_592_000.0, LeUnit::Raw),
            "<2.592e+06"
        );
        // +Inf passes through verbatim regardless of unit.
        assert_eq!(format_le("+Inf", f64::INFINITY, LeUnit::Duration), "+Inf");
        assert_eq!(format_le("+Inf", f64::INFINITY, LeUnit::Raw), "+Inf");
    }

    #[test]
    fn format_count_whole_vs_fractional() {
        assert_eq!(format_count(1_539.0), "1539");
        assert_eq!(format_count(0.0), "0");
        assert_eq!(format_count(0.0889), "0.0889");
    }

    #[test]
    fn vector_by_le_rejects_non_vector() {
        let data = json!({"resultType": "matrix", "result": []});
        let err = vector_by_le(&data).unwrap_err();
        assert!(format!("{err}").contains("expected a vector"));
    }

    #[test]
    fn vector_by_le_rejects_missing_le() {
        let data = json!({
            "resultType": "vector",
            "result": [{"metric": {}, "value": [1.0, "5"]}]
        });
        let err = vector_by_le(&data).unwrap_err();
        assert!(format!("{err}").contains("`le` label"));
    }

    #[test]
    fn vector_by_le_rejects_non_numeric_value() {
        let data = json!({
            "resultType": "vector",
            "result": [{"metric": {"le": "1.0"}, "value": [1.0, "not-a-number"]}]
        });
        let err = vector_by_le(&data).unwrap_err();
        assert!(format!("{err}").contains("not numeric"));
    }

    /// The headline behaviour: cumulative + rate vectors merge into
    /// `le`-sorted rows, `+Inf` last, deltas being the cumulative diff.
    #[test]
    fn build_buckets_merges_sorts_and_deltas() {
        let cum = json!({
            "resultType": "vector",
            "result": [
                {"metric": {"le": "+Inf"}, "value": [1.0, "100"]},
                {"metric": {"le": "1.0"}, "value": [1.0, "10"]},
                {"metric": {"le": "5.0"}, "value": [1.0, "30"]}
            ]
        });
        let rate = json!({
            "resultType": "vector",
            "result": [
                {"metric": {"le": "1.0"}, "value": [1.0, "0.5"]},
                {"metric": {"le": "5.0"}, "value": [1.0, "1.5"]}
            ]
        });
        let rows = build_buckets(&cum, &rate).unwrap();
        assert_eq!(rows.len(), 3);

        // Sorted ascending by le, +Inf last.
        assert_eq!(rows[0].le_str, "1.0");
        assert_eq!(rows[1].le_str, "5.0");
        assert_eq!(rows[2].le_str, "+Inf");

        // Cumulative counts verbatim; deltas are the running difference.
        assert_eq!(rows[0].cum, 10.0);
        assert_eq!(rows[0].delta, 10.0);
        assert_eq!(rows[1].cum, 30.0);
        assert_eq!(rows[1].delta, 20.0);
        assert_eq!(rows[2].cum, 100.0);
        assert_eq!(rows[2].delta, 70.0);

        // Rate present where the rate query had the bucket, None for +Inf.
        assert_eq!(rows[0].rate, Some(0.5));
        assert_eq!(rows[1].rate, Some(1.5));
        assert_eq!(rows[2].rate, None);
    }

    #[test]
    fn format_bucket_row_renders_all_columns() {
        let row = BucketRow {
            le_str: "2.592e+06".to_string(),
            le_val: 2_592_000.0,
            cum: 2_517.0,
            delta: 978.0,
            rate: Some(0.0889),
        };
        assert_eq!(
            format_bucket_row(&row, LeUnit::Duration),
            "le=<30.00d\tcum=2517\tdelta=978\trate=0.0889/s"
        );
    }

    #[test]
    fn format_bucket_row_missing_rate_is_dash() {
        let row = BucketRow {
            le_str: "+Inf".to_string(),
            le_val: f64::INFINITY,
            cum: 100.0,
            delta: 70.0,
            rate: None,
        };
        assert_eq!(
            format_bucket_row(&row, LeUnit::Duration),
            "le=+Inf\tcum=100\tdelta=70\trate=-"
        );
    }
}
