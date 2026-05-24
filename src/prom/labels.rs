//! `sak prom labels` — list all label names known to a Prometheus server.
//!
//! Queries `/api/v1/labels` and emits one label name per line, sorted
//! ascending for diff-stable output. This is the "what dimensions can I
//! group by?" entry point an LLM reaches for first on an unfamiliar Prom.

use std::process::ExitCode;

use anyhow::{Result, anyhow};
use clap::Args;
use serde_json::Value;

use crate::prom::common_args::CommonPromArgs;
use crate::prom::runner::run_prom;

#[derive(Args)]
#[command(
    about = "List all label names",
    long_about = "List every label name known to the Prometheus server from \
        `/api/v1/labels`. One label per line, sorted ascending.\n\n\
        Useful as a discovery entry point — pair with \
        `sak prom label-values <name>` to enumerate the values of a label.\n\n\
        Connection: pass --url <http://prom:9090> or set PROMETHEUS_URL.",
    after_help = "\
Examples:
  sak prom labels                                Every label name on the server
  sak prom labels --json                         Raw JSON for piping
  sak prom labels --limit 50                     First 50 labels"
)]
pub struct LabelsArgs {
    #[command(flatten)]
    pub common: CommonPromArgs,
}

pub fn run(args: &LabelsArgs) -> Result<ExitCode> {
    run_prom(&args.common, "/api/v1/labels", |data| {
        let mut names = extract_strings(data, "/api/v1/labels")?;
        names.sort();
        Ok(names)
    })
}

/// Extract the JSON `data` field as a `Vec<String>`. Shared by
/// [`labels::run`] and [`crate::prom::label_values::run`] since both
/// endpoints return the same shape (`data` is an array of strings).
///
/// `pub(super)` so `label_values` can reuse without duplicating the
/// error-shape boilerplate.
pub(super) fn extract_strings(data: &Value, path: &str) -> Result<Vec<String>> {
    let arr = data
        .as_array()
        .ok_or_else(|| anyhow!("Prometheus {path} `data` is not an array"))?;
    let mut out = Vec::with_capacity(arr.len());
    for v in arr {
        let s = v
            .as_str()
            .ok_or_else(|| anyhow!("Prometheus {path} `data` element is not a string: {v:?}"))?;
        out.push(s.to_string());
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn extract_strings_basic() {
        let data = json!(["__name__", "job", "instance"]);
        let v = extract_strings(&data, "/api/v1/labels").unwrap();
        assert_eq!(v, vec!["__name__", "job", "instance"]);
    }

    #[test]
    fn extract_strings_empty() {
        let data = json!([]);
        let v = extract_strings(&data, "/api/v1/labels").unwrap();
        assert!(v.is_empty());
    }

    #[test]
    fn extract_strings_errors_on_non_array() {
        let err = extract_strings(&json!({"x": 1}), "/api/v1/labels").unwrap_err();
        assert!(format!("{err}").contains("not an array"));
    }

    #[test]
    fn extract_strings_errors_on_non_string_element() {
        let err = extract_strings(&json!(["ok", 42]), "/api/v1/labels").unwrap_err();
        assert!(format!("{err}").contains("not a string"));
    }
}
