//! `sak loki labels` — list all label names known to a Loki server.
//!
//! Queries `/loki/api/v1/labels` and emits one label name per line, sorted
//! ascending for diff-stable output. This is the "what streams can I select
//! on?" entry point for an unfamiliar Loki — pair it with
//! `sak loki label-values <name>` to enumerate a label's values.
//!
//! Note: Loki scopes `/labels` to a default recent time window (typically the
//! last few hours) when no `start`/`end` is given, so very old label names may
//! not appear. The instant/range query commands are the lever for older data.

use crate::output::Outcome;

use anyhow::{Result, anyhow};
use clap::Args;
use serde_json::Value;

use crate::loki::common_args::CommonLokiArgs;
use crate::loki::runner::run_loki;

#[derive(Args)]
#[command(
    about = "List all label names",
    long_about = "List every label name known to the Loki server from \
        `/loki/api/v1/labels`. One label per line, sorted ascending.\n\n\
        Useful as a discovery entry point — pair with \
        `sak loki label-values <name>` to enumerate the values of a label.\n\n\
        Loki scopes this to a default recent time window when no range is \
        given, so very old labels may be absent.\n\n\
        Connection: pass --url <http://loki:3100> or set LOKI_URL.",
    after_help = "\
Examples:
  sak loki labels                                Every label name on the server
  sak loki labels --json                         Raw JSON for piping
  sak loki labels --limit 50                     First 50 labels"
)]
pub struct LabelsArgs {
    #[command(flatten)]
    pub common: CommonLokiArgs,
}

pub fn run(args: &LabelsArgs) -> Result<Outcome> {
    run_loki(&args.common, "/loki/api/v1/labels", |data| {
        let mut names = extract_strings(data, "/loki/api/v1/labels")?;
        names.sort();
        Ok(names)
    })
}

/// Extract the JSON `data` field as a `Vec<String>`. Shared by [`labels::run`]
/// and [`crate::loki::label_values::run`] since both endpoints return the same
/// shape (`data` is an array of strings).
///
/// `pub(super)` so `label_values` can reuse without duplicating the
/// error-shape boilerplate.
pub(super) fn extract_strings(data: &Value, path: &str) -> Result<Vec<String>> {
    let arr = data
        .as_array()
        .ok_or_else(|| anyhow!("Loki {path} `data` is not an array"))?;
    let mut out = Vec::with_capacity(arr.len());
    for v in arr {
        let s = v
            .as_str()
            .ok_or_else(|| anyhow!("Loki {path} `data` element is not a string: {v:?}"))?;
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
        let data = json!(["app", "namespace", "level"]);
        let v = extract_strings(&data, "/loki/api/v1/labels").unwrap();
        assert_eq!(v, vec!["app", "namespace", "level"]);
    }

    #[test]
    fn extract_strings_empty() {
        let data = json!([]);
        let v = extract_strings(&data, "/loki/api/v1/labels").unwrap();
        assert!(v.is_empty());
    }

    #[test]
    fn extract_strings_errors_on_non_array() {
        let err = extract_strings(&json!({"x": 1}), "/loki/api/v1/labels").unwrap_err();
        assert!(format!("{err}").contains("not an array"));
    }

    #[test]
    fn extract_strings_errors_on_non_string_element() {
        let err = extract_strings(&json!(["ok", 42]), "/loki/api/v1/labels").unwrap_err();
        assert!(format!("{err}").contains("not a string"));
    }
}
