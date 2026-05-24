//! `sak prom query <promql>` — instant PromQL query against `/api/v1/query`.
//!
//! Output shape depends on the response's `resultType`:
//! - `scalar` / `string`: one line `<value>`.
//! - `vector`: one line per series, `<labels><TAB><value>`.
//! - `matrix`: one line per (series, sample) pair,
//!   `<labels><TAB><ts><TAB><value>`. Matrix from an instant query is
//!   uncommon (subquery expressions emit it) but we handle it for parity
//!   with `query-range`, which will reuse the same formatter.
//!
//! Labels are serialized in the Prometheus canonical form
//! `{key="value",...}`, sorted by key for determinism. Output rows are
//! also sorted, so re-running the same query against an unchanged Prom
//! produces identical text — critical for LLM diff stability.

use std::fmt::Write as _;
use std::process::ExitCode;

use anyhow::{Result, anyhow, bail};
use clap::Args;
use serde_json::Value;

use crate::prom::common_args::CommonPromArgs;
use crate::prom::runner::run_prom;

#[derive(Args)]
#[command(
    about = "Run an instant PromQL query",
    long_about = "Execute a PromQL instant query against `/api/v1/query` and \
        print one line per result. Vector results emit \
        `<labels><TAB><value>`; scalar/string results emit just the value; \
        matrix results (from subquery expressions) emit \
        `<labels><TAB><ts><TAB><value>` per sample.\n\n\
        Labels are serialized in Prometheus canonical form \
        `{key=\"value\",...}` and sorted, so the same query against an \
        unchanged Prom produces identical output.\n\n\
        Connection: pass --url <http://prom:9090> or set PROMETHEUS_URL.",
    after_help = "\
Examples:
  sak prom query 'up'                              Every up{} series
  sak prom query 'sum(rate(http_requests[5m]))'   Aggregate
  sak prom query 'up{job=\"node\"}' --json         Raw JSON for piping
  sak prom query 'up' --url http://prom:9090       Override PROMETHEUS_URL"
)]
pub struct QueryArgs {
    #[command(flatten)]
    pub common: CommonPromArgs,

    /// The PromQL expression to evaluate
    #[arg(value_name = "PROMQL")]
    pub query: String,
}

pub fn run(args: &QueryArgs) -> Result<ExitCode> {
    let path = format!("/api/v1/query?query={}", urlencode(&args.query));
    run_prom(&args.common, &path, |data| {
        let mut lines = format_result(data)?;
        lines.sort();
        Ok(lines)
    })
}

/// Render a Prometheus query response payload (`{resultType, result}`) as
/// zero or more output lines. Pure so it's unit-testable on hand-built
/// fixtures with no live server.
///
/// `pub(super)` so the upcoming `sak prom query-range` command can reuse
/// the same formatter (a range query always returns `resultType=matrix`,
/// which this function already handles).
pub(super) fn format_result(data: &Value) -> Result<Vec<String>> {
    let result_type = data
        .get("resultType")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("Prometheus query response has no `resultType`"))?;
    let result = data
        .get("result")
        .ok_or_else(|| anyhow!("Prometheus query response has no `result`"))?;

    match result_type {
        "scalar" | "string" => format_scalar(result),
        "vector" => format_vector(result),
        "matrix" => format_matrix(result),
        other => bail!("unknown Prometheus resultType {other:?}"),
    }
}

/// Format a `scalar`/`string` result (a single sample pair) as one value line.
fn format_scalar(result: &Value) -> Result<Vec<String>> {
    let (_ts, val) = parse_sample_pair(result)?;
    Ok(vec![val])
}

/// Format a `vector` result (one sample per series) as `labels<TAB>value` lines.
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

/// Parse a Prometheus sample pair `[<ts>, "<value>"]`. The timestamp is
/// either a JSON number (modern Prom) or a JSON string (some forks); the
/// value is always a string. Returns `(ts, value)` with the timestamp
/// rendered as a decimal string so output is uniform.
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

/// Format a label set as `{key="value",...}` in Prometheus canonical form.
/// Keys sorted ascending for determinism. Empty / missing renders as `{}`.
///
/// `pub(super)` so range / histogram commands (to be added in subsequent
/// commits) can reuse this serialization.
pub(super) fn format_labels(metric: Option<&Value>) -> String {
    let map = match metric.and_then(Value::as_object) {
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
/// `\n`. Matches the upstream Prometheus serialization so the output can
/// be re-parsed by other Prom-aware tooling.
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
/// pulling in a dedicated URL crate just to escape PromQL braces.
///
/// `pub(super)` so `sak prom query-range` and `sak prom histogram` reuse
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
        let m = json!({"job": "node", "instance": "1.2.3.4:9100", "alertname": "X"});
        assert_eq!(
            format_labels(Some(&m)),
            r#"{alertname="X",instance="1.2.3.4:9100",job="node"}"#
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
    fn format_labels_escapes_newline() {
        let m = json!({"err": "line1\nline2"});
        assert_eq!(format_labels(Some(&m)), r#"{err="line1\nline2"}"#);
    }

    #[test]
    fn format_result_vector() {
        let data = json!({
            "resultType": "vector",
            "result": [
                {
                    "metric": {"__name__": "up", "instance": "1.1.1.1:9100"},
                    "value": [1715587200.0, "1"]
                },
                {
                    "metric": {"__name__": "up", "instance": "2.2.2.2:9100"},
                    "value": [1715587200.0, "0"]
                }
            ]
        });
        let lines = format_result(&data).unwrap();
        assert_eq!(lines.len(), 2);
        assert!(
            lines
                .iter()
                .any(|l| l.contains("1.1.1.1:9100") && l.ends_with("\t1"))
        );
        assert!(
            lines
                .iter()
                .any(|l| l.contains("2.2.2.2:9100") && l.ends_with("\t0"))
        );
    }

    #[test]
    fn format_result_scalar() {
        let data = json!({"resultType": "scalar", "result": [1715587200.0, "42"]});
        let lines = format_result(&data).unwrap();
        assert_eq!(lines, vec!["42"]);
    }

    #[test]
    fn format_result_string() {
        let data = json!({"resultType": "string", "result": [1715587200.0, "hello"]});
        let lines = format_result(&data).unwrap();
        assert_eq!(lines, vec!["hello"]);
    }

    #[test]
    fn format_result_matrix() {
        let data = json!({
            "resultType": "matrix",
            "result": [{
                "metric": {"job": "node"},
                "values": [[1715587200.0, "1"], [1715587260.0, "2"]]
            }]
        });
        let lines = format_result(&data).unwrap();
        assert_eq!(lines.len(), 2);
        assert!(lines[0].ends_with("\t1"));
        assert!(lines[1].ends_with("\t2"));
        assert!(lines[0].starts_with("{job=\"node\"}"));
    }

    #[test]
    fn format_result_unknown_type_errors() {
        let data = json!({"resultType": "weird", "result": null});
        let err = format_result(&data).unwrap_err();
        assert!(format!("{err}").contains("unknown Prometheus resultType"));
    }

    #[test]
    fn format_result_missing_result_type_errors() {
        let data = json!({"result": []});
        let err = format_result(&data).unwrap_err();
        assert!(format!("{err}").contains("`resultType`"));
    }

    #[test]
    fn parse_sample_pair_accepts_string_timestamp() {
        let v = json!(["1715587200.000", "42"]);
        let (ts, val) = parse_sample_pair(&v).unwrap();
        assert_eq!(ts, "1715587200.000");
        assert_eq!(val, "42");
    }

    #[test]
    fn parse_sample_pair_rejects_wrong_length() {
        let v = json!(["1715587200", "42", "extra"]);
        let err = parse_sample_pair(&v).unwrap_err();
        assert!(format!("{err}").contains("length 3"));
    }

    #[test]
    fn urlencode_handles_promql_punctuation() {
        assert_eq!(urlencode("foo bar"), "foo%20bar");
        assert_eq!(
            urlencode("sum(up{job=\"node\"})"),
            "sum%28up%7Bjob%3D%22node%22%7D%29"
        );
        assert_eq!(urlencode("a-b_c.d~e"), "a-b_c.d~e");
    }

    #[test]
    fn urlencode_handles_non_ascii() {
        // µ = U+00B5, encoded UTF-8 as C2 B5
        assert_eq!(urlencode("µs"), "%C2%B5s");
    }
}
