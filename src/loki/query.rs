//! `sak loki query <logql>` — instant LogQL query against `/loki/api/v1/query`.
//!
//! Output shape depends on the response's `resultType`:
//! - `streams` (a log selector): one line per log entry,
//!   `<ts_ns><TAB><labels><TAB><line>`. This is the common case.
//! - `vector` (an instant metric query, e.g. `count_over_time(...)`): one line
//!   per series, `<labels><TAB><value>`.
//! - `matrix` (a range metric query — uncommon from the instant endpoint, but
//!   handled for parity with `query-range`, which reuses this formatter):
//!   one line per (series, sample) pair, `<labels><TAB><ts><TAB><value>`.
//!
//! Labels are serialized in the canonical form `{key="value",...}`, sorted by
//! key for determinism. Output rows are also sorted, so re-running the same
//! query against an unchanged Loki produces identical text — critical for LLM
//! diff stability. Log lines run through [`crate::output::collapse_ws`] so an
//! embedded newline or tab can't break the one-entry-per-row contract.

use crate::output::Outcome;
use std::fmt::Write as _;

use anyhow::{Result, anyhow, bail};
use clap::Args;
use serde_json::Value;

use crate::loki::common_args::CommonLokiArgs;
use crate::loki::runner::run_loki;
use crate::output::collapse_ws;

#[derive(Args)]
#[command(
    about = "Run an instant LogQL query",
    long_about = "Execute a LogQL instant query against `/loki/api/v1/query` \
        and print the most recent matching log entries. A log selector returns \
        streams — one `<ts_ns><TAB><labels><TAB><line>` line per entry. A \
        metric LogQL expression (e.g. `count_over_time({app=\"api\"}[5m])`) \
        returns a vector — one `<labels><TAB><value>` line per series.\n\n\
        Labels are serialized in canonical form `{key=\"value\",...}` and \
        sorted, and log lines are flattened to one row each, so the same query \
        against an unchanged Loki produces identical output.\n\n\
        Connection: pass --url <http://loki:3100> or set LOKI_URL.",
    after_help = "\
Examples:
  sak loki query '{app=\"api\"}'                     Recent lines for one app
  sak loki query '{app=\"api\"} |= \"error\"'          Lines containing `error`
  sak loki query 'count_over_time({app=\"api\"}[5m])' Per-stream count (metric)
  sak loki query '{app=\"api\"}' --json              Raw JSON for piping
  sak loki query '{app=\"api\"}' --url http://loki:3100"
)]
pub struct QueryArgs {
    #[command(flatten)]
    pub common: CommonLokiArgs,

    /// The LogQL expression to evaluate
    #[arg(value_name = "LOGQL")]
    pub query: String,
}

pub fn run(args: &QueryArgs) -> Result<Outcome> {
    let path = format!("/loki/api/v1/query?query={}", urlencode(&args.query));
    run_loki(&args.common, &path, |data| {
        let mut lines = format_result(data)?;
        lines.sort();
        Ok(lines)
    })
}

/// Render a Loki query response payload (`{resultType, result}`) as zero or
/// more output lines. Pure so it's unit-testable on hand-built fixtures with
/// no live server.
///
/// `pub(super)` so `sak loki query-range` can reuse the same formatter (a
/// range log query also returns `resultType=streams`; a range metric query
/// returns `matrix`, which this function already handles).
pub(super) fn format_result(data: &Value) -> Result<Vec<String>> {
    let result_type = data
        .get("resultType")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("Loki query response has no `resultType`"))?;
    let result = data
        .get("result")
        .ok_or_else(|| anyhow!("Loki query response has no `result`"))?;

    match result_type {
        "streams" => format_streams(result),
        "vector" => format_vector(result),
        "matrix" => format_matrix(result),
        other => bail!("unknown Loki resultType {other:?}"),
    }
}

/// Format a `streams` result (log entries grouped by label set) as
/// `ts_ns<TAB>labels<TAB>line` lines, one per entry. The per-stream label set
/// lives in `stream`; each entry in `values` is `[<ts_ns_string>, "<line>"]`.
fn format_streams(result: &Value) -> Result<Vec<String>> {
    let streams = result
        .as_array()
        .ok_or_else(|| anyhow!("streams `result` is not an array"))?;
    let mut out = Vec::new();
    for s in streams {
        let labels = format_labels(s.get("stream"));
        let values = s
            .get("values")
            .and_then(Value::as_array)
            .ok_or_else(|| anyhow!("stream has no `values` array"))?;
        for entry in values {
            let (ts, line) = parse_stream_entry(entry)?;
            out.push(format!("{ts}\t{labels}\t{}", collapse_ws(&line)));
        }
    }
    Ok(out)
}

/// Format a `vector` result (one sample per series) as `labels<TAB>value`
/// lines. Mirrors the Prometheus vector shape — a metric LogQL query returns
/// the same `[{metric, value:[ts, "v"]}]` structure.
fn format_vector(result: &Value) -> Result<Vec<String>> {
    let series = result
        .as_array()
        .ok_or_else(|| anyhow!("vector `result` is not an array"))?;
    let mut out = Vec::with_capacity(series.len());
    for s in series {
        let labels = format_labels(s.get("metric"));
        let pair = s
            .get("value")
            .ok_or_else(|| anyhow!("vector series has no `value`"))?;
        let (_ts, val) = parse_sample_pair(pair)?;
        out.push(format!("{labels}\t{val}"));
    }
    Ok(out)
}

/// Format a `matrix` result (a time series per series) as
/// `labels<TAB>timestamp<TAB>value` lines, one per sample.
fn format_matrix(result: &Value) -> Result<Vec<String>> {
    let series = result
        .as_array()
        .ok_or_else(|| anyhow!("matrix `result` is not an array"))?;
    let mut out = Vec::new();
    for s in series {
        let labels = format_labels(s.get("metric"));
        let values = s
            .get("values")
            .and_then(Value::as_array)
            .ok_or_else(|| anyhow!("matrix series has no `values` array"))?;
        for sample in values {
            let (ts, val) = parse_sample_pair(sample)?;
            out.push(format!("{labels}\t{ts}\t{val}"));
        }
    }
    Ok(out)
}

/// Parse a Loki stream entry `["<ts_ns>", "<line>"]`. Both elements are
/// strings (the timestamp is a nanosecond Unix epoch rendered as a decimal
/// string). Returns `(ts, line)`.
fn parse_stream_entry(v: &Value) -> Result<(String, String)> {
    let arr = v
        .as_array()
        .ok_or_else(|| anyhow!("expected stream entry [\"<ts>\", \"<line>\"], got {v:?}"))?;
    if arr.len() != 2 {
        bail!(
            "expected stream entry of length 2, got length {}",
            arr.len()
        );
    }
    let ts = arr[0]
        .as_str()
        .ok_or_else(|| anyhow!("stream entry timestamp is not a string: {v:?}"))?
        .to_string();
    let line = arr[1]
        .as_str()
        .ok_or_else(|| anyhow!("stream entry line is not a string: {v:?}"))?
        .to_string();
    Ok((ts, line))
}

/// Parse a metric sample pair `[<ts>, "<value>"]`. The timestamp is either a
/// JSON number (seconds, possibly fractional) or a JSON string; the value is
/// always a string. Returns `(ts, value)` with the timestamp rendered as a
/// decimal string so output is uniform.
fn parse_sample_pair(v: &Value) -> Result<(String, String)> {
    let arr = v
        .as_array()
        .ok_or_else(|| anyhow!("expected sample [<ts>, \"<value>\"], got {v:?}"))?;
    if arr.len() != 2 {
        bail!("expected sample of length 2, got length {}", arr.len());
    }
    let ts = match &arr[0] {
        Value::Number(n) => n.to_string(),
        Value::String(s) => s.clone(),
        other => bail!("sample timestamp is not a number or string: {other:?}"),
    };
    let val = arr[1]
        .as_str()
        .ok_or_else(|| anyhow!("sample value is not a string: {v:?}"))?
        .to_string();
    Ok((ts, val))
}

/// Format a label set as `{key="value",...}` in canonical form. Keys sorted
/// ascending for determinism. Empty / missing renders as `{}`.
///
/// `pub(super)` so the other Loki commands (range, label discovery) reuse this
/// serialization.
pub(super) fn format_labels(labels: Option<&Value>) -> String {
    let map = match labels.and_then(Value::as_object) {
        Some(m) if !m.is_empty() => m,
        _ => return "{}".to_string(),
    };
    let mut entries: Vec<(&String, &Value)> = map.iter().collect();
    entries.sort_by(|a, b| a.0.cmp(b.0));
    let mut out = String::from("{");
    for (i, (k, v)) in entries.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        let raw = v.as_str().unwrap_or("");
        write!(out, "{}=\"{}\"", k, escape_label_value(raw)).unwrap();
    }
    out.push('}');
    out
}

/// Escape the inside of a label value: `\` → `\\`, `"` → `\"`, newline →
/// `\n`. Matches the upstream Prometheus/Loki serialization so the output can
/// be re-parsed by other LogQL-aware tooling.
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

/// Minimal `application/x-www-form-urlencoded` encoder for query parameter
/// values. Encodes anything outside the unreserved set (RFC 3986). Avoids
/// pulling in a dedicated URL crate just to escape LogQL braces.
///
/// `pub(super)` so `sak loki query-range`, `label-values`, and `series` reuse
/// the same encoder when building their request paths.
pub(super) fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for byte in s.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char);
            }
            _ => write!(&mut out, "%{:02X}", byte).unwrap(),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn format_labels_canonical_sorted() {
        let m = json!({"app": "api", "level": "info", "ns": "prod"});
        assert_eq!(
            format_labels(Some(&m)),
            r#"{app="api",level="info",ns="prod"}"#
        );
    }

    #[test]
    fn format_labels_empty_or_missing() {
        assert_eq!(format_labels(None), "{}");
        assert_eq!(format_labels(Some(&json!({}))), "{}");
    }

    #[test]
    fn format_labels_escapes_special_chars() {
        let m = json!({"path": "a/b\"c\\d"});
        assert_eq!(format_labels(Some(&m)), r#"{path="a/b\"c\\d"}"#);
    }

    #[test]
    fn format_result_streams() {
        let data = json!({
            "resultType": "streams",
            "result": [
                {
                    "stream": {"app": "api", "level": "info"},
                    "values": [
                        ["1700000000000000000", "started up"],
                        ["1700000000000000001", "ready"]
                    ]
                }
            ]
        });
        let lines = format_result(&data).unwrap();
        assert_eq!(lines.len(), 2);
        assert_eq!(
            lines[0],
            "1700000000000000000\t{app=\"api\",level=\"info\"}\tstarted up"
        );
        assert_eq!(
            lines[1],
            "1700000000000000001\t{app=\"api\",level=\"info\"}\tready"
        );
    }

    #[test]
    fn format_streams_flattens_multiline_log_line() {
        let data = json!({
            "resultType": "streams",
            "result": [{
                "stream": {"app": "api"},
                "values": [["1700000000000000000", "line1\nline2\twith tab"]]
            }]
        });
        let lines = format_result(&data).unwrap();
        // The embedded newline and tab in the log line are collapsed to spaces
        // so the `ts<TAB>labels<TAB>line` contract holds — exactly three tabs'
        // worth of structure (two separators) on the row.
        assert_eq!(lines[0].matches('\t').count(), 2);
        assert_eq!(
            lines[0],
            "1700000000000000000\t{app=\"api\"}\tline1 line2 with tab"
        );
    }

    #[test]
    fn format_result_vector() {
        let data = json!({
            "resultType": "vector",
            "result": [
                {"metric": {"app": "api"}, "value": [1700000000.0, "5"]},
                {"metric": {"app": "web"}, "value": [1700000000.0, "9"]}
            ]
        });
        let lines = format_result(&data).unwrap();
        assert_eq!(lines.len(), 2);
        assert!(lines.iter().any(|l| l == r#"{app="api"}	5"#));
        assert!(lines.iter().any(|l| l == r#"{app="web"}	9"#));
    }

    #[test]
    fn format_result_matrix() {
        let data = json!({
            "resultType": "matrix",
            "result": [{
                "metric": {"app": "api"},
                "values": [[1700000000.0, "1"], [1700000060.0, "2"]]
            }]
        });
        let lines = format_result(&data).unwrap();
        assert_eq!(lines.len(), 2);
        assert!(lines[0].ends_with("\t1"));
        assert!(lines[1].ends_with("\t2"));
        assert!(lines[0].starts_with("{app=\"api\"}"));
    }

    #[test]
    fn format_result_unknown_type_errors() {
        let data = json!({"resultType": "weird", "result": null});
        let err = format_result(&data).unwrap_err();
        assert!(format!("{err}").contains("unknown Loki resultType"));
    }

    #[test]
    fn format_result_missing_result_type_errors() {
        let data = json!({"result": []});
        let err = format_result(&data).unwrap_err();
        assert!(format!("{err}").contains("`resultType`"));
    }

    #[test]
    fn parse_stream_entry_basic() {
        let v = json!(["1700000000000000000", "hello"]);
        let (ts, line) = parse_stream_entry(&v).unwrap();
        assert_eq!(ts, "1700000000000000000");
        assert_eq!(line, "hello");
    }

    #[test]
    fn parse_stream_entry_rejects_wrong_length() {
        let v = json!(["1700000000000000000"]);
        let err = parse_stream_entry(&v).unwrap_err();
        assert!(format!("{err}").contains("length 1"));
    }

    #[test]
    fn parse_stream_entry_rejects_non_string_ts() {
        let v = json!([1700000000000000000_i64, "hello"]);
        let err = parse_stream_entry(&v).unwrap_err();
        assert!(format!("{err}").contains("timestamp is not a string"));
    }

    #[test]
    fn parse_sample_pair_accepts_string_timestamp() {
        let v = json!(["1700000000.000", "42"]);
        let (ts, val) = parse_sample_pair(&v).unwrap();
        assert_eq!(ts, "1700000000.000");
        assert_eq!(val, "42");
    }

    #[test]
    fn urlencode_handles_logql_punctuation() {
        assert_eq!(urlencode("foo bar"), "foo%20bar");
        assert_eq!(urlencode("{app=\"api\"}"), "%7Bapp%3D%22api%22%7D");
        assert_eq!(urlencode("a-b_c.d~e"), "a-b_c.d~e");
    }

    #[test]
    fn urlencode_handles_non_ascii() {
        // µ = U+00B5, encoded UTF-8 as C2 B5
        assert_eq!(urlencode("µs"), "%C2%B5s");
    }
}
