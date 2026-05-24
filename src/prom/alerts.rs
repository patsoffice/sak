//! `sak prom alerts` — list alerts on a Prometheus server.
//!
//! Queries `/api/v1/alerts` and renders one alert per line as
//! `alertname<TAB>severity<TAB>instance<TAB>value<TAB>state<TAB>activeAt<TAB>summary`,
//! sorted by `(state, alertname, instance)` for determinism.
//!
//! Default state filter is firing + pending — what an operator usually
//! wants on triage. Use `--all` to include any other states the endpoint
//! returns, `--firing` or `--pending` to narrow further. Multi-line
//! `summary` annotations are collapsed to one space-separated line so each
//! alert stays one row (mirrors the `sak k8s events` pattern).

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
    about = "List alerts on a Prometheus server",
    long_about = "List alerts from `/api/v1/alerts`. Default state filter is \
        firing+pending; use --all to include any other states the endpoint \
        returns, or --firing / --pending to narrow further. Use --name \
        <regex> to filter by alertname.\n\n\
        Connection: pass --url <http://prom:9090> or set PROMETHEUS_URL. \
        Auto-discovery via Kubernetes service + transparent port-forward is \
        a planned follow-up.",
    after_help = "\
Examples:
  sak prom alerts                                      Firing+pending in current Prom
  sak prom alerts --all                                Every alert returned
  sak prom alerts --firing                             Firing only
  sak prom alerts --name 'Cert.*'                      Filter by alertname regex
  sak prom alerts --url http://prom:9090 --json        Raw JSON for piping"
)]
pub struct AlertsArgs {
    #[command(flatten)]
    pub common: CommonPromArgs,

    /// Show only firing alerts
    #[arg(long, conflicts_with_all = ["pending", "all"])]
    pub firing: bool,

    /// Show only pending alerts
    #[arg(long, conflicts_with_all = ["firing", "all"])]
    pub pending: bool,

    /// Show every alert returned regardless of state
    #[arg(long, conflicts_with_all = ["firing", "pending"])]
    pub all: bool,

    /// Filter by alertname regex
    #[arg(long, value_name = "REGEX")]
    pub name: Option<String>,
}

/// One row extracted from a Prometheus alert object. Pure data so it can be
/// unit-tested on hand-built fixtures with no live server.
pub(super) struct AlertRow {
    pub alertname: String,
    pub severity: String,
    pub instance: String,
    pub value: String,
    pub state: String,
    pub active_at: String,
    pub summary: String,
}

/// Pull a row from a single alert object. Missing fields render as `-`
/// rather than being dropped, so a malformed alert still appears in the
/// output (mirrors `sak k8s events`).
pub(super) fn extract_alert_row(alert: &Value) -> AlertRow {
    let labels = alert.get("labels");
    let annotations = alert.get("annotations");
    AlertRow {
        alertname: label(labels, "alertname").unwrap_or("-").to_string(),
        severity: label(labels, "severity").unwrap_or("-").to_string(),
        instance: label(labels, "instance").unwrap_or("-").to_string(),
        value: extract_value(alert.get("value")),
        state: alert
            .get("state")
            .and_then(Value::as_str)
            .unwrap_or("-")
            .to_string(),
        active_at: alert
            .get("activeAt")
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

/// Prometheus emits the alert `value` as a string in modern releases, but
/// some older versions / forks emit a number. Accept both; everything else
/// becomes `-`.
fn extract_value(v: Option<&Value>) -> String {
    match v {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Number(n)) => n.to_string(),
        _ => "-".to_string(),
    }
}

/// Format one row as the `alertname<TAB>...<TAB>summary` line emitted by
/// `sak prom alerts`. Multi-line summaries collapse to one space-separated
/// line so each alert stays one output row.
pub(super) fn format_alert_row(row: &AlertRow) -> String {
    format!(
        "{}\t{}\t{}\t{}\t{}\t{}\t{}",
        row.alertname,
        row.severity,
        row.instance,
        row.value,
        row.state,
        row.active_at,
        collapse_newlines(&row.summary)
    )
}

/// Sort by `(state, alertname, instance)` for determinism.
pub(super) fn sort_rows(rows: &mut [AlertRow]) {
    rows.sort_by(|a, b| {
        a.state
            .cmp(&b.state)
            .then_with(|| a.alertname.cmp(&b.alertname))
            .then_with(|| a.instance.cmp(&b.instance))
    });
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum StateFilter {
    All,
    Firing,
    Pending,
    FiringPending,
}

impl StateFilter {
    pub(super) fn allows(&self, state: &str) -> bool {
        match self {
            StateFilter::All => true,
            StateFilter::Firing => state == "firing",
            StateFilter::Pending => state == "pending",
            StateFilter::FiringPending => state == "firing" || state == "pending",
        }
    }
}

/// Resolve the state filter from the mutually-exclusive CLI flags. The
/// `conflicts_with_all` attributes on `AlertsArgs` mean at most one of
/// `firing` / `pending` / `all` is ever set; the default is firing+pending.
pub(super) fn state_filter(args: &AlertsArgs) -> StateFilter {
    if args.all {
        StateFilter::All
    } else if args.firing {
        StateFilter::Firing
    } else if args.pending {
        StateFilter::Pending
    } else {
        StateFilter::FiringPending
    }
}

pub fn run(args: &AlertsArgs) -> Result<ExitCode> {
    run_prom(&args.common, "/api/v1/alerts", |data| {
        let alerts = data
            .get("alerts")
            .and_then(Value::as_array)
            .ok_or_else(|| anyhow!("Prometheus /api/v1/alerts data has no `alerts` array"))?;

        let filter = state_filter(args);
        let name_re = match &args.name {
            Some(s) => Some(Regex::new(s).map_err(|e| anyhow!("invalid --name regex: {e}"))?),
            None => None,
        };

        let mut rows: Vec<AlertRow> = alerts
            .iter()
            .map(extract_alert_row)
            .filter(|r| filter.allows(&r.state))
            .filter(|r| name_re.as_ref().is_none_or(|re| re.is_match(&r.alertname)))
            .collect();
        sort_rows(&mut rows);
        Ok(rows.iter().map(format_alert_row).collect())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn extract_row_basic() {
        let alert = json!({
            "labels": {
                "alertname": "CertExpiringSoon",
                "severity": "warning",
                "instance": "1.1.1.1:6443"
            },
            "annotations": {"summary": "cert in <30 days"},
            "state": "firing",
            "activeAt": "2026-05-13T01:00:00Z",
            "value": "1.234e+05"
        });
        let row = extract_alert_row(&alert);
        assert_eq!(row.alertname, "CertExpiringSoon");
        assert_eq!(row.severity, "warning");
        assert_eq!(row.instance, "1.1.1.1:6443");
        assert_eq!(row.state, "firing");
        assert_eq!(row.value, "1.234e+05");
        assert_eq!(row.summary, "cert in <30 days");
        assert_eq!(row.active_at, "2026-05-13T01:00:00Z");
    }

    #[test]
    fn extract_row_missing_fields_use_dashes() {
        let alert = json!({});
        let row = extract_alert_row(&alert);
        assert_eq!(row.alertname, "-");
        assert_eq!(row.severity, "-");
        assert_eq!(row.instance, "-");
        assert_eq!(row.value, "-");
        assert_eq!(row.state, "-");
        assert_eq!(row.active_at, "-");
        assert_eq!(row.summary, "");
    }

    #[test]
    fn extract_row_accepts_numeric_value() {
        let alert = json!({"value": 42});
        let row = extract_alert_row(&alert);
        assert_eq!(row.value, "42");
    }

    #[test]
    fn state_filter_default_is_firing_pending() {
        let f = StateFilter::FiringPending;
        assert!(f.allows("firing"));
        assert!(f.allows("pending"));
        assert!(!f.allows("inactive"));
    }

    #[test]
    fn state_filter_all_allows_anything() {
        let f = StateFilter::All;
        assert!(f.allows("firing"));
        assert!(f.allows("pending"));
        assert!(f.allows("inactive"));
        assert!(f.allows("weird"));
    }

    #[test]
    fn state_filter_firing_only() {
        let f = StateFilter::Firing;
        assert!(f.allows("firing"));
        assert!(!f.allows("pending"));
    }

    #[test]
    fn format_emits_tab_separated_line() {
        let row = AlertRow {
            alertname: "X".into(),
            severity: "warning".into(),
            instance: "y".into(),
            value: "1".into(),
            state: "firing".into(),
            active_at: "2026-05-13T00:00:00Z".into(),
            summary: "s".into(),
        };
        assert_eq!(
            format_alert_row(&row),
            "X\twarning\ty\t1\tfiring\t2026-05-13T00:00:00Z\ts"
        );
    }

    #[test]
    fn format_collapses_multiline_summary() {
        let row = AlertRow {
            alertname: "X".into(),
            severity: "-".into(),
            instance: "-".into(),
            value: "-".into(),
            state: "firing".into(),
            active_at: "-".into(),
            summary: "line1\nline2\rline3".into(),
        };
        let line = format_alert_row(&row);
        assert!(line.contains("line1 line2 line3"));
    }

    #[test]
    fn sort_orders_by_state_then_alertname_then_instance() {
        let mut rows = vec![
            row("B", "firing", "i1"),
            row("A", "firing", "i2"),
            row("C", "pending", "i0"),
            row("A", "firing", "i1"),
        ];
        sort_rows(&mut rows);
        assert_eq!(
            rows.iter()
                .map(|r| (r.alertname.as_str(), r.state.as_str(), r.instance.as_str()))
                .collect::<Vec<_>>(),
            vec![
                ("A", "firing", "i1"),
                ("A", "firing", "i2"),
                ("B", "firing", "i1"),
                ("C", "pending", "i0"),
            ]
        );
    }

    fn row(alertname: &str, state: &str, instance: &str) -> AlertRow {
        AlertRow {
            alertname: alertname.into(),
            severity: "-".into(),
            instance: instance.into(),
            value: "-".into(),
            state: state.into(),
            active_at: "-".into(),
            summary: "".into(),
        }
    }
}
