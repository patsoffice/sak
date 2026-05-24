//! `sak prom metadata [metric]` — metric metadata (type, help, unit).
//!
//! Queries `/api/v1/metadata` (or `/api/v1/metadata?metric=<name>`) and
//! emits one line per `(metric, entry)` pair as
//! `metric<TAB>type<TAB>unit<TAB>help`, sorted by `(metric, type, help)`
//! for diff-stable output. One metric can have multiple entries (different
//! scrape targets exposing slightly different help text), and every entry
//! is emitted — collapsing them silently would hide the discrepancy.

use std::process::ExitCode;

use anyhow::{Result, anyhow};
use clap::Args;
use serde_json::Value;

use crate::prom::common_args::CommonPromArgs;
use crate::prom::output::collapse_newlines;
use crate::prom::query::urlencode;
use crate::prom::runner::run_prom;

#[derive(Args)]
#[command(
    about = "Metric metadata (type, help, unit)",
    long_about = "Query `/api/v1/metadata` for type / help / unit metadata. \
        With no positional, returns metadata for every metric currently \
        scraped; pass <metric> to narrow to one. Output is one line per \
        `(metric, entry)` pair: `metric<TAB>type<TAB>unit<TAB>help`, sorted \
        by (metric, type, help).\n\n\
        A metric can have multiple entries when different scrape targets \
        expose slightly different help text — every entry is emitted so \
        the discrepancy is visible.\n\n\
        Connection: pass --url <http://prom:9090> or set PROMETHEUS_URL.",
    after_help = "\
Examples:
  sak prom metadata                              All scraped metrics
  sak prom metadata up                           One metric
  sak prom metadata --json                       Raw JSON for piping
  sak prom metadata --limit 100                  First 100 lines"
)]
pub struct MetadataArgs {
    #[command(flatten)]
    pub common: CommonPromArgs,

    /// Optional metric name to narrow to (passed as `?metric=<name>`)
    #[arg(value_name = "METRIC")]
    pub metric: Option<String>,
}

/// One row extracted from a metadata entry. Pure data so the walking and
/// formatting logic is unit-testable on hand-built fixtures. `Debug` so
/// `extract_metadata_rows(...).unwrap_err()` works in the tests.
#[derive(Debug)]
pub(super) struct MetadataRow {
    pub metric: String,
    pub metric_type: String,
    pub unit: String,
    pub help: String,
}

pub fn run(args: &MetadataArgs) -> Result<ExitCode> {
    let path = match &args.metric {
        Some(m) => format!("/api/v1/metadata?metric={}", urlencode(m)),
        None => "/api/v1/metadata".to_string(),
    };
    run_prom(&args.common, &path, |data| {
        let mut rows = extract_metadata_rows(data)?;
        sort_rows(&mut rows);
        Ok(rows.iter().map(format_metadata_row).collect())
    })
}

/// Walk the metadata `data` object, emitting one [`MetadataRow`] per
/// `(metric, entry)` pair. The response shape is
/// `{ "<metric>": [{"type":..., "help":..., "unit":...}, ...] }`.
pub(super) fn extract_metadata_rows(data: &Value) -> Result<Vec<MetadataRow>> {
    let obj = data
        .as_object()
        .ok_or_else(|| anyhow!("Prometheus /api/v1/metadata `data` is not an object"))?;
    let mut rows = Vec::new();
    for (metric, entries) in obj {
        let arr = entries.as_array().ok_or_else(|| {
            anyhow!("Prometheus /api/v1/metadata entry for {metric:?} is not an array")
        })?;
        for entry in arr {
            rows.push(MetadataRow {
                metric: metric.clone(),
                metric_type: str_or(entry.get("type"), "-"),
                unit: str_or(entry.get("unit"), ""),
                help: str_or(entry.get("help"), ""),
            });
        }
    }
    Ok(rows)
}

fn str_or(v: Option<&Value>, default: &str) -> String {
    v.and_then(Value::as_str).unwrap_or(default).to_string()
}

/// Format one row as `metric<TAB>type<TAB>unit<TAB>help`. The `help` text
/// is newline-collapsed so each entry stays on one output row.
pub(super) fn format_metadata_row(row: &MetadataRow) -> String {
    format!(
        "{}\t{}\t{}\t{}",
        row.metric,
        row.metric_type,
        row.unit,
        collapse_newlines(&row.help)
    )
}

/// Sort by `(metric, type, help)`. `help` is the final tiebreaker because
/// the same metric can have entries that differ only in help text — a
/// stable order makes diffs against a previous run readable.
pub(super) fn sort_rows(rows: &mut [MetadataRow]) {
    rows.sort_by(|a, b| {
        a.metric
            .cmp(&b.metric)
            .then_with(|| a.metric_type.cmp(&b.metric_type))
            .then_with(|| a.help.cmp(&b.help))
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn sample() -> Value {
        json!({
            "up": [
                {"type": "gauge", "help": "1 if scrape succeeded", "unit": ""}
            ],
            "go_gc_duration_seconds": [
                {"type": "summary", "help": "A summary of GC pauses.", "unit": "seconds"},
                {"type": "summary", "help": "A summary of garbage collection.", "unit": "seconds"}
            ]
        })
    }

    #[test]
    fn extract_walks_every_entry() {
        let rows = extract_metadata_rows(&sample()).unwrap();
        assert_eq!(rows.len(), 3);
    }

    #[test]
    fn extract_preserves_per_metric_entries() {
        let rows = extract_metadata_rows(&sample()).unwrap();
        let gc: Vec<_> = rows
            .iter()
            .filter(|r| r.metric == "go_gc_duration_seconds")
            .collect();
        assert_eq!(gc.len(), 2);
        assert!(gc.iter().all(|r| r.metric_type == "summary"));
        assert!(gc.iter().any(|r| r.help == "A summary of GC pauses."));
        assert!(
            gc.iter()
                .any(|r| r.help == "A summary of garbage collection.")
        );
    }

    #[test]
    fn extract_missing_fields_default() {
        let data = json!({"x": [{}]});
        let rows = extract_metadata_rows(&data).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].metric, "x");
        assert_eq!(rows[0].metric_type, "-");
        assert_eq!(rows[0].unit, "");
        assert_eq!(rows[0].help, "");
    }

    #[test]
    fn extract_errors_on_non_object_data() {
        let err = extract_metadata_rows(&json!([])).unwrap_err();
        assert!(format!("{err}").contains("not an object"));
    }

    #[test]
    fn extract_errors_when_entries_not_array() {
        let err = extract_metadata_rows(&json!({"x": "not-an-array"})).unwrap_err();
        assert!(format!("{err}").contains("not an array"));
    }

    #[test]
    fn format_emits_tab_separated_line() {
        let row = MetadataRow {
            metric: "up".into(),
            metric_type: "gauge".into(),
            unit: "".into(),
            help: "1 if scrape succeeded".into(),
        };
        assert_eq!(
            format_metadata_row(&row),
            "up\tgauge\t\t1 if scrape succeeded"
        );
    }

    #[test]
    fn format_collapses_multiline_help() {
        let row = MetadataRow {
            metric: "x".into(),
            metric_type: "counter".into(),
            unit: "".into(),
            help: "line1\nline2".into(),
        };
        assert!(format_metadata_row(&row).contains("line1 line2"));
    }

    #[test]
    fn sort_orders_by_metric_type_help() {
        let mut rows = vec![
            row("b", "counter", "h"),
            row("a", "gauge", "z"),
            row("a", "gauge", "a"),
        ];
        sort_rows(&mut rows);
        let names: Vec<_> = rows
            .iter()
            .map(|r| (r.metric.as_str(), r.help.as_str()))
            .collect();
        assert_eq!(names, vec![("a", "a"), ("a", "z"), ("b", "h")]);
    }

    fn row(metric: &str, mtype: &str, help: &str) -> MetadataRow {
        MetadataRow {
            metric: metric.into(),
            metric_type: mtype.into(),
            unit: "".into(),
            help: help.into(),
        }
    }
}
