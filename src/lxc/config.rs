//! `sak lxc config` — show the configuration subset of an LXD/Incus instance.
//!
//! Issues a `GET /1.0/instances/<name>` against the discovered unix socket via
//! [`super::client::LxcClient::get_json`] and emits a pretty-printed JSON
//! object containing only the configuration-relevant keys: `config`, `devices`,
//! `profiles`, `expanded_config`, and `expanded_devices`. The runtime `state`
//! block (covered by `sak lxc info`) is intentionally excluded.
//!
//! With `--path`, the same dot-notation / JSON Pointer machinery used by
//! `sak k8s get --path` and the `json` domain is applied to the subset, so a
//! caller can drill straight to a single key (`--path .config."image.os"`)
//! without piping through `sak json query`.
//!
//! A 404 from the daemon (instance not found) maps to exit code 1; any other
//! error is exit code 2 with a message on stderr.

use std::io;
use std::process::ExitCode;

use anyhow::Result;
use clap::Args;
use serde_json::{Map, Value};

use crate::lxc::client::LxcClient;
use crate::output::BoundedWriter;
use crate::value::{format_value, resolve_expression};

/// Keys copied from the raw instance metadata into the config subset.
/// Output ordering is determined by `serde_json::Map` (alphabetical, since
/// the `preserve_order` feature is not enabled crate-wide).
const CONFIG_KEYS: &[&str] = &[
    "config",
    "devices",
    "profiles",
    "expanded_config",
    "expanded_devices",
];

#[derive(Args)]
#[command(
    about = "Show the configuration of an LXD/Incus instance",
    long_about = "Show the configuration subset of a single LXD or Incus \
        instance: `config`, `devices`, `profiles`, `expanded_config`, and \
        `expanded_devices`. The runtime `state` block is excluded — use \
        `sak lxc info` for that.\n\n\
        Output is pretty-printed JSON. With --path, the same dot-notation or \
        JSON Pointer expression supported by `sak k8s get --path` is applied \
        to the subset and only the extracted value is emitted. Exit code 1 if \
        the instance does not exist; exit code 2 on any other error.",
    after_help = "\
Examples:
  sak lxc config web1                            Configuration subset as JSON
  sak lxc config web1 --project mylab            Look up in a specific project
  sak lxc config web1 --path .config             Just the user config map
  sak lxc config web1 --path .expanded_devices.eth0
                                                 One device from the expanded view
  sak lxc config web1 --path /profiles           JSON Pointer also works"
)]
pub struct ConfigArgs {
    /// Instance name
    pub name: String,

    /// LXD project to look the instance up in (default: `default`)
    #[arg(long)]
    pub project: Option<String>,

    /// Extract a field via dot notation (`.config`) or JSON Pointer
    /// (`/config`)
    #[arg(long)]
    pub path: Option<String>,

    /// Maximum number of output lines
    #[arg(long)]
    pub limit: Option<usize>,
}

pub async fn run(args: &ConfigArgs) -> Result<ExitCode> {
    let client = LxcClient::connect()?;

    let path = match &args.project {
        Some(p) => format!("/1.0/instances/{}?project={p}", args.name),
        None => format!("/1.0/instances/{}", args.name),
    };

    let metadata = match client.get_json(&path).await? {
        Some(v) => v,
        None => return Ok(ExitCode::from(1)),
    };

    let subset = config_subset(&metadata);

    let stdout = io::stdout();
    let handle = stdout.lock();
    let mut writer = BoundedWriter::new(handle, args.limit);

    let mut wrote_any = false;
    if let Some(expr) = &args.path {
        if let Some(extracted) = resolve_expression(&subset, expr)? {
            let formatted = format_value(extracted, false, true);
            for line in formatted.split('\n') {
                if !writer.write_line(line)? {
                    break;
                }
                wrote_any = true;
            }
        }
    } else {
        let pretty = serde_json::to_string_pretty(&subset)?;
        for line in pretty.lines() {
            if !writer.write_line(line)? {
                break;
            }
            wrote_any = true;
        }
    }

    writer.flush()?;
    if wrote_any {
        Ok(ExitCode::SUCCESS)
    } else {
        Ok(ExitCode::from(1))
    }
}

/// Project the configuration-relevant keys out of an instance metadata
/// object. Missing keys are simply skipped — older LXD versions and freshly
/// created instances may not populate the `expanded_*` fields.
fn config_subset(metadata: &Value) -> Value {
    let mut out = Map::new();
    if let Some(obj) = metadata.as_object() {
        for key in CONFIG_KEYS {
            if let Some(v) = obj.get(*key) {
                out.insert((*key).to_string(), v.clone());
            }
        }
    }
    Value::Object(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn config_subset_keeps_only_configured_keys() {
        let meta = json!({
            "name": "web1",
            "status": "Running",
            "config": {"image.os": "Debian"},
            "devices": {"eth0": {"type": "nic"}},
            "profiles": ["default"],
            "expanded_config": {"image.os": "Debian", "limits.cpu": "2"},
            "expanded_devices": {"eth0": {"type": "nic", "network": "lxdbr0"}},
            "state": {"network": {}}
        });
        let subset = config_subset(&meta);
        let obj = subset.as_object().unwrap();
        assert_eq!(obj.len(), 5);
        assert!(obj.contains_key("config"));
        assert!(obj.contains_key("devices"));
        assert!(obj.contains_key("profiles"));
        assert!(obj.contains_key("expanded_config"));
        assert!(obj.contains_key("expanded_devices"));
        assert!(!obj.contains_key("state"));
        assert!(!obj.contains_key("name"));
    }

    #[test]
    fn config_subset_skips_missing_keys() {
        let meta = json!({
            "name": "fresh",
            "config": {},
            "devices": {}
        });
        let subset = config_subset(&meta);
        let obj = subset.as_object().unwrap();
        assert_eq!(obj.len(), 2);
        assert!(obj.contains_key("config"));
        assert!(obj.contains_key("devices"));
    }

    #[test]
    fn config_subset_handles_non_object_input() {
        let subset = config_subset(&json!("oops"));
        assert!(subset.as_object().unwrap().is_empty());
    }
}
