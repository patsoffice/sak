//! `sak prom tsdb-stats` — top-K cardinality offenders.
//!
//! Queries `/api/v1/status/tsdb` and emits the head-block summary plus the
//! four top-K arrays Prometheus already returns (sorted by series count
//! descending server-side). Output is one line per row, tagged with the
//! section name so each section is greppable:
//!
//! ```text
//! head<TAB>numSeries<TAB>508
//! head<TAB>numLabelPairs<TAB>1234
//! series_by_metric<TAB>net_conntrack_dialer_conn_failed_total<TAB>20
//! label_values_by_name<TAB>id<TAB>30
//! memory_by_label<TAB>id<TAB>240
//! series_by_label_pair<TAB>__name__=node_filesystem_size_bytes<TAB>100
//! ```
//!
//! This is the command you reach for during a "why is Prometheus
//! OOM-ing?" incident — the response is the same data the built-in
//! `/tsdb-status` UI page renders.

use crate::output::Outcome;

use anyhow::{Result, anyhow};
use clap::Args;
use serde_json::Value;

use crate::prom::common_args::CommonPromArgs;
use crate::prom::runner::run_prom;

#[derive(Args)]
#[command(
    about = "Top-K cardinality offenders (TSDB status)",
    long_about = "Query `/api/v1/status/tsdb` and emit one tab-separated row \
        per stat: `section<TAB>name<TAB>value`. Sections are `head` \
        (head-block summary: numSeries, numLabelPairs, chunkCount, minTime, \
        maxTime), `series_by_metric` (seriesCountByMetricName), \
        `label_values_by_name` (labelValueCountByLabelName), \
        `memory_by_label` (memoryInBytesByLabelName), and \
        `series_by_label_pair` (seriesCountByLabelValuePair). \
        Section order and within-section order are preserved from the \
        upstream response, which Prometheus already sorts by value desc.\n\n\
        Connection: pass --url <http://prom:9090> or set PROMETHEUS_URL.",
    after_help = "\
Examples:
  sak prom tsdb-stats                                   Full TSDB status
  sak prom tsdb-stats --json                            Raw JSON for piping
  sak prom tsdb-stats --limit 20                        First 20 rows
  sak prom tsdb-stats | sak fs grep series_by_metric    Just one section"
)]
pub struct TsdbStatsArgs {
    #[command(flatten)]
    pub common: CommonPromArgs,
}

pub fn run(args: &TsdbStatsArgs) -> Result<Outcome> {
    run_prom(&args.common, "/api/v1/status/tsdb", |data| {
        extract_tsdb_lines(data)
    })
}

/// Sections in the order they appear in the output. `head` first because
/// it's the summary; the four top-K arrays follow in the order an operator
/// usually walks them when triaging cardinality blowups.
const SECTIONS: &[(&str, &str)] = &[
    ("series_by_metric", "seriesCountByMetricName"),
    ("label_values_by_name", "labelValueCountByLabelName"),
    ("memory_by_label", "memoryInBytesByLabelName"),
    ("series_by_label_pair", "seriesCountByLabelValuePair"),
];

/// Build the `section<TAB>name<TAB>value` lines. Pure so it's unit-testable
/// on hand-built fixtures. Missing sections are silently skipped — different
/// Prometheus releases expose slightly different subsets and the command
/// should degrade gracefully rather than fail the whole listing.
pub(super) fn extract_tsdb_lines(data: &Value) -> Result<Vec<String>> {
    let obj = data
        .as_object()
        .ok_or_else(|| anyhow!("Prometheus /api/v1/status/tsdb `data` is not an object"))?;
    let mut lines = Vec::new();

    if let Some(head) = obj.get("headStats").and_then(Value::as_object) {
        // Preserve insertion order from serde_json (preserve-order is on
        // the workspace defaults), but fall back to a fixed order for
        // determinism if the keys ever come back in a different sequence.
        for key in [
            "numSeries",
            "numLabelPairs",
            "chunkCount",
            "minTime",
            "maxTime",
        ] {
            if let Some(v) = head.get(key) {
                lines.push(format!("head\t{key}\t{}", value_to_string(v)));
            }
        }
    }

    for (section, json_key) in SECTIONS {
        let Some(arr) = obj.get(*json_key).and_then(Value::as_array) else {
            continue;
        };
        for entry in arr {
            let name = entry.get("name").and_then(Value::as_str).unwrap_or("-");
            let value = entry
                .get("value")
                .map(value_to_string)
                .unwrap_or_else(|| "-".to_string());
            lines.push(format!("{section}\t{name}\t{value}"));
        }
    }

    Ok(lines)
}

/// Render a JSON scalar (number, string, bool) as a flat string. Anything
/// non-scalar collapses to its serde_json representation — the TSDB status
/// values are always scalars in practice, but the fallback keeps the
/// row contract intact if Prometheus ever adds richer fields.
fn value_to_string(v: &Value) -> String {
    match v {
        Value::Number(n) => n.to_string(),
        Value::String(s) => s.clone(),
        Value::Bool(b) => b.to_string(),
        Value::Null => "-".to_string(),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn sample() -> Value {
        json!({
            "headStats": {
                "numSeries": 508,
                "numLabelPairs": 1234,
                "chunkCount": 555,
                "minTime": 1_715_000_000_000_i64,
                "maxTime": 1_715_000_100_000_i64
            },
            "seriesCountByMetricName": [
                {"name": "net_conntrack_dialer_conn_failed_total", "value": 20},
                {"name": "http_requests_total", "value": 12}
            ],
            "labelValueCountByLabelName": [
                {"name": "id", "value": 30}
            ],
            "memoryInBytesByLabelName": [
                {"name": "id", "value": 240}
            ],
            "seriesCountByLabelValuePair": [
                {"name": "__name__=node_filesystem_size_bytes", "value": 100}
            ]
        })
    }

    #[test]
    fn extract_emits_head_summary() {
        let lines = extract_tsdb_lines(&sample()).unwrap();
        assert!(lines.contains(&"head\tnumSeries\t508".to_string()));
        assert!(lines.contains(&"head\tnumLabelPairs\t1234".to_string()));
        assert!(lines.contains(&"head\tchunkCount\t555".to_string()));
    }

    #[test]
    fn extract_emits_each_section_in_declared_order() {
        let lines = extract_tsdb_lines(&sample()).unwrap();
        let body: Vec<_> = lines
            .iter()
            .filter(|l| !l.starts_with("head\t"))
            .map(|l| l.split('\t').next().unwrap().to_string())
            .collect();
        let unique_in_order: Vec<_> = body.iter().fold(Vec::new(), |mut acc, s| {
            if acc.last().is_none_or(|last: &String| last != s) {
                acc.push(s.clone());
            }
            acc
        });
        assert_eq!(
            unique_in_order,
            vec![
                "series_by_metric",
                "label_values_by_name",
                "memory_by_label",
                "series_by_label_pair",
            ]
        );
    }

    #[test]
    fn extract_preserves_upstream_within_section_order() {
        // Prometheus already sorts by value desc; we keep that order.
        let lines = extract_tsdb_lines(&sample()).unwrap();
        let metric_rows: Vec<_> = lines
            .iter()
            .filter(|l| l.starts_with("series_by_metric\t"))
            .collect();
        assert_eq!(metric_rows.len(), 2);
        assert!(metric_rows[0].contains("net_conntrack_dialer_conn_failed_total"));
        assert!(metric_rows[1].contains("http_requests_total"));
    }

    #[test]
    fn extract_skips_missing_sections() {
        let data = json!({"headStats": {"numSeries": 1}});
        let lines = extract_tsdb_lines(&data).unwrap();
        assert_eq!(lines, vec!["head\tnumSeries\t1".to_string()]);
    }

    #[test]
    fn extract_errors_on_non_object_data() {
        let err = extract_tsdb_lines(&json!([])).unwrap_err();
        assert!(format!("{err}").contains("not an object"));
    }
}
