//! `sak prom flags` — daemon command-line flags.
//!
//! Queries `/api/v1/status/flags` and emits one `flag<TAB>value` line per
//! configured Prometheus flag, sorted ascending by flag name. The values
//! are stringly typed by the API (Prometheus serializes every flag as a
//! string, even booleans), so the format passes them through verbatim.

use std::process::ExitCode;

use anyhow::{Result, anyhow};
use clap::Args;
use serde_json::Value;

use crate::prom::common_args::CommonPromArgs;
use crate::prom::output::collapse_newlines;
use crate::prom::runner::run_prom;

#[derive(Args)]
#[command(
    about = "Daemon command-line flags",
    long_about = "List the runtime flags Prometheus was started with, from \
        `/api/v1/status/flags`. One `flag<TAB>value` line per flag, sorted \
        ascending by name.\n\n\
        Values are passed through verbatim (Prometheus serializes every \
        flag as a string in this endpoint, even booleans and durations).\n\n\
        Connection: pass --url <http://prom:9090> or set PROMETHEUS_URL.",
    after_help = "\
Examples:
  sak prom flags                                 All runtime flags
  sak prom flags --json                          Raw JSON for piping
  sak prom flags | sak fs grep retention         Just retention-related flags"
)]
pub struct FlagsArgs {
    #[command(flatten)]
    pub common: CommonPromArgs,
}

pub fn run(args: &FlagsArgs) -> Result<ExitCode> {
    run_prom(&args.common, "/api/v1/status/flags", |data| {
        let mut rows = extract_flag_rows(data)?;
        rows.sort_by(|a, b| a.0.cmp(&b.0));
        Ok(rows
            .iter()
            .map(|(flag, value)| format!("{flag}\t{}", collapse_newlines(value)))
            .collect())
    })
}

/// Pull `(flag, value)` pairs from the flags object. Non-string values
/// (Prometheus has never emitted any, but be defensive) collapse to their
/// serde_json representation so the row contract stays intact.
pub(super) fn extract_flag_rows(data: &Value) -> Result<Vec<(String, String)>> {
    let obj = data
        .as_object()
        .ok_or_else(|| anyhow!("Prometheus /api/v1/status/flags `data` is not an object"))?;
    let mut rows = Vec::with_capacity(obj.len());
    for (k, v) in obj {
        let value = match v {
            Value::String(s) => s.clone(),
            other => other.to_string(),
        };
        rows.push((k.clone(), value));
    }
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn extract_basic() {
        let data = json!({
            "storage.tsdb.retention.time": "15d",
            "web.enable-lifecycle": "false"
        });
        let rows = extract_flag_rows(&data).unwrap();
        let map: std::collections::HashMap<_, _> = rows.into_iter().collect();
        assert_eq!(map["storage.tsdb.retention.time"], "15d");
        assert_eq!(map["web.enable-lifecycle"], "false");
    }

    #[test]
    fn extract_handles_non_string_value_defensively() {
        let data = json!({"some-numeric-flag": 42});
        let rows = extract_flag_rows(&data).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].0, "some-numeric-flag");
        assert_eq!(rows[0].1, "42");
    }

    #[test]
    fn extract_errors_on_non_object() {
        let err = extract_flag_rows(&json!([])).unwrap_err();
        assert!(format!("{err}").contains("not an object"));
    }
}
