//! `sak prom config` — Prometheus runtime YAML config.
//!
//! Queries `/api/v1/status/config` and writes the embedded YAML blob to
//! stdout verbatim, line by line through [`BoundedWriter`]. The endpoint's
//! envelope is `{status:"success", data:{yaml:"<full yaml>"}}` — we strip
//! the envelope and the one-key wrapper and pipe the YAML directly so it
//! can be re-parsed by `sak config ...` if desired.
//!
//! The file lives under `src/prom/prom_config.rs` (not `src/prom/config.rs`)
//! to keep visual distance from the top-level `src/config/` domain — the
//! command name `sak prom config` is still unambiguous on the CLI.

use std::process::ExitCode;

use anyhow::{Result, anyhow};
use clap::Args;
use serde_json::Value;

use crate::prom::common_args::CommonPromArgs;
use crate::prom::runner::run_prom;

#[derive(Args)]
#[command(
    about = "Prometheus runtime YAML config",
    long_about = "Fetch the daemon's effective YAML configuration from \
        `/api/v1/status/config`. By default the YAML blob is written to \
        stdout verbatim (one line per YAML line through the bounded \
        writer) so it can be piped into `sak config ...` for further \
        inspection. Use --json to emit the raw `{yaml: ...}` wrapper \
        instead.\n\n\
        Connection: pass --url <http://prom:9090> or set PROMETHEUS_URL.",
    after_help = "\
Examples:
  sak prom config                                Full prometheus.yml
  sak prom config | sak config query .scrape_configs -f yaml
  sak prom config --json                         Raw JSON wrapper"
)]
pub struct ConfigArgs {
    #[command(flatten)]
    pub common: CommonPromArgs,
}

pub fn run(args: &ConfigArgs) -> Result<ExitCode> {
    run_prom(&args.common, "/api/v1/status/config", |data| {
        let yaml = extract_yaml(data)?;
        Ok(yaml.lines().map(str::to_string).collect())
    })
}

/// Pull the embedded YAML blob out of the `{yaml: "..."}` wrapper. Pure
/// so it's unit-testable on hand-built fixtures.
pub(super) fn extract_yaml(data: &Value) -> Result<String> {
    let yaml = data
        .get("yaml")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("Prometheus /api/v1/status/config has no `yaml` string"))?;
    Ok(yaml.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn extract_yaml_basic() {
        let data = json!({"yaml": "global:\n  scrape_interval: 15s\n"});
        let s = extract_yaml(&data).unwrap();
        assert!(s.starts_with("global:"));
        assert!(s.contains("scrape_interval: 15s"));
    }

    #[test]
    fn extract_yaml_errors_when_missing() {
        let err = extract_yaml(&json!({})).unwrap_err();
        assert!(format!("{err}").contains("`yaml` string"));
    }

    #[test]
    fn extract_yaml_errors_when_not_string() {
        let err = extract_yaml(&json!({"yaml": 42})).unwrap_err();
        assert!(format!("{err}").contains("`yaml` string"));
    }
}
