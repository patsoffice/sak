//! `sak prom targets [--down] [--job <regex>]` — list scrape targets.
//!
//! Queries `/api/v1/targets` and renders one active target per line as
//! `health<TAB>job<TAB>instance<TAB>scrapeUrl<TAB>lastScrape<TAB>lastError`,
//! sorted by `(job, instance)` for determinism.
//!
//! Only `activeTargets` is shown — `droppedTargets` (discovered, then
//! relabeled away) are intentionally omitted; they're rarely what you're
//! triaging. `--down` narrows to targets whose health is not `up`;
//! `--job <regex>` filters by the `job` label. Multi-line `lastError`
//! strings collapse to one row.

use std::process::ExitCode;

use anyhow::{Result, anyhow};
use clap::Args;
use regex::Regex;
use serde_json::Value;

use crate::prom::common_args::CommonPromArgs;
use crate::prom::output::collapse_newlines;
use crate::prom::runner::run_prom;

#[derive(Args)]
#[command(
    about = "List scrape targets on a Prometheus server",
    long_about = "List active scrape targets from `/api/v1/targets`, one per \
        line as `health<TAB>job<TAB>instance<TAB>scrapeUrl<TAB>lastScrape\
        <TAB>lastError`, sorted by (job, instance).\n\n\
        Only activeTargets are shown; droppedTargets (discovered, then \
        relabeled away) are omitted. Use --down to narrow to targets whose \
        health is not `up`, and --job <regex> to filter by job label.\n\n\
        Connection: pass --url <http://prom:9090> or set PROMETHEUS_URL.",
    after_help = "\
Examples:
  sak prom targets                              All active targets
  sak prom targets --down                       Only unhealthy targets
  sak prom targets --job 'node.*'               Targets whose job matches
  sak prom targets --down --json                Raw JSON for piping"
)]
pub struct TargetsArgs {
    #[command(flatten)]
    pub common: CommonPromArgs,

    /// Show only targets whose health is not `up`
    #[arg(long)]
    pub down: bool,

    /// Filter by job label regex
    #[arg(long, value_name = "REGEX")]
    pub job: Option<String>,
}

/// One row extracted from an active scrape target. Pure data so it can be
/// unit-tested on hand-built fixtures with no live server.
pub(super) struct TargetRow {
    pub health: String,
    pub job: String,
    pub instance: String,
    pub scrape_url: String,
    pub last_scrape: String,
    pub last_error: String,
}

/// Pull a row from a single active target. Missing fields render as `-`
/// (or empty, for `lastError`) rather than being dropped, so a malformed
/// target still appears in the output.
pub(super) fn extract_target_row(target: &Value) -> TargetRow {
    let labels = target.get("labels");
    TargetRow {
        health: str_or(target.get("health"), "-"),
        job: label(labels, "job").unwrap_or("-").to_string(),
        instance: label(labels, "instance").unwrap_or("-").to_string(),
        scrape_url: str_or(target.get("scrapeUrl"), "-"),
        last_scrape: str_or(target.get("lastScrape"), "-"),
        last_error: str_or(target.get("lastError"), ""),
    }
}

fn label<'a>(labels: Option<&'a Value>, key: &str) -> Option<&'a str> {
    labels.and_then(|l| l.get(key)).and_then(Value::as_str)
}

fn str_or(v: Option<&Value>, default: &str) -> String {
    v.and_then(Value::as_str).unwrap_or(default).to_string()
}

/// Format one row as the
/// `health<TAB>job<TAB>instance<TAB>scrapeUrl<TAB>lastScrape<TAB>lastError`
/// line. The `lastError` is newline-collapsed so each target stays one row.
pub(super) fn format_target_row(row: &TargetRow) -> String {
    format!(
        "{}\t{}\t{}\t{}\t{}\t{}",
        row.health,
        row.job,
        row.instance,
        row.scrape_url,
        row.last_scrape,
        collapse_newlines(&row.last_error)
    )
}

/// Sort by `(job, instance)` for deterministic output.
pub(super) fn sort_rows(rows: &mut [TargetRow]) {
    rows.sort_by(|a, b| a.job.cmp(&b.job).then_with(|| a.instance.cmp(&b.instance)));
}

pub fn run(args: &TargetsArgs) -> Result<ExitCode> {
    run_prom(&args.common, "/api/v1/targets", |data| {
        let active = data
            .get("activeTargets")
            .and_then(Value::as_array)
            .ok_or_else(|| {
                anyhow!("Prometheus /api/v1/targets data has no `activeTargets` array")
            })?;

        let job_re = match &args.job {
            Some(s) => Some(Regex::new(s).map_err(|e| anyhow!("invalid --job regex: {e}"))?),
            None => None,
        };

        let mut rows: Vec<TargetRow> = active
            .iter()
            .map(extract_target_row)
            .filter(|r| !args.down || r.health != "up")
            .filter(|r| job_re.as_ref().is_none_or(|re| re.is_match(&r.job)))
            .collect();
        sort_rows(&mut rows);
        Ok(rows.iter().map(format_target_row).collect())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn extract_row_basic() {
        let target = json!({
            "labels": {"job": "node-exporter", "instance": "10.0.0.1:9100"},
            "scrapeUrl": "http://10.0.0.1:9100/metrics",
            "lastScrape": "2026-05-14T12:00:00Z",
            "lastError": "",
            "health": "up"
        });
        let row = extract_target_row(&target);
        assert_eq!(row.health, "up");
        assert_eq!(row.job, "node-exporter");
        assert_eq!(row.instance, "10.0.0.1:9100");
        assert_eq!(row.scrape_url, "http://10.0.0.1:9100/metrics");
        assert_eq!(row.last_scrape, "2026-05-14T12:00:00Z");
        assert_eq!(row.last_error, "");
    }

    #[test]
    fn extract_row_missing_fields_use_dashes() {
        let row = extract_target_row(&json!({}));
        assert_eq!(row.health, "-");
        assert_eq!(row.job, "-");
        assert_eq!(row.instance, "-");
        assert_eq!(row.scrape_url, "-");
        assert_eq!(row.last_scrape, "-");
        assert_eq!(row.last_error, "");
    }

    #[test]
    fn format_emits_tab_separated_line() {
        let row = TargetRow {
            health: "down".into(),
            job: "node".into(),
            instance: "h:9100".into(),
            scrape_url: "http://h:9100/metrics".into(),
            last_scrape: "2026-05-14T12:00:00Z".into(),
            last_error: "connection refused".into(),
        };
        assert_eq!(
            format_target_row(&row),
            "down\tnode\th:9100\thttp://h:9100/metrics\t2026-05-14T12:00:00Z\tconnection refused"
        );
    }

    #[test]
    fn format_collapses_multiline_last_error() {
        let row = TargetRow {
            health: "down".into(),
            job: "-".into(),
            instance: "-".into(),
            scrape_url: "-".into(),
            last_scrape: "-".into(),
            last_error: "dial tcp:\nconnection refused".into(),
        };
        assert!(format_target_row(&row).contains("dial tcp: connection refused"));
    }

    #[test]
    fn sort_orders_by_job_then_instance() {
        let mut rows = vec![row("node", "h2"), row("apiserver", "h1"), row("node", "h1")];
        sort_rows(&mut rows);
        assert_eq!(
            rows.iter()
                .map(|r| (r.job.as_str(), r.instance.as_str()))
                .collect::<Vec<_>>(),
            vec![("apiserver", "h1"), ("node", "h1"), ("node", "h2")]
        );
    }

    fn row(job: &str, instance: &str) -> TargetRow {
        TargetRow {
            health: "up".into(),
            job: job.into(),
            instance: instance.into(),
            scrape_url: "-".into(),
            last_scrape: "-".into(),
            last_error: "".into(),
        }
    }
}
