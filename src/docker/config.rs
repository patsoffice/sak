//! `sak docker config` — show the configuration subset of a Docker container.
//!
//! Issues a `GET /containers/<id-or-name>/json` against the discovered unix
//! socket via [`super::client::DockerClient::get_json`] and emits a
//! pretty-printed JSON object containing only the configuration-relevant
//! sub-objects: `Config` and `HostConfig`. The runtime `State` block (covered
//! by `sak docker info`) and other fields like `NetworkSettings` and `Mounts`
//! are intentionally excluded.
//!
//! With `--path`, the same dot-notation / JSON Pointer machinery used by
//! `sak k8s get --path` and the `json` domain is applied to the subset, so a
//! caller can drill straight to a single key (`--path .Config.Image`) without
//! piping through `sak json query`.
//!
//! A 404 from the daemon (container not found) maps to exit code 1; any other
//! error is exit code 2 with a message on stderr.

use std::io;
use std::process::ExitCode;

use anyhow::Result;
use clap::Args;
use serde_json::{Map, Value};

use crate::docker::client::DockerClient;
use crate::output::BoundedWriter;
use crate::value::{format_value, resolve_expression};

/// Keys copied from the raw container metadata into the config subset.
const CONFIG_KEYS: &[&str] = &["Config", "HostConfig"];

#[derive(Args)]
#[command(
    about = "Show the configuration of a Docker container",
    long_about = "Show the configuration subset of a single Docker container: \
        `Config` (image, env, cmd, entrypoint, labels, ...) and `HostConfig` \
        (binds, port bindings, restart policy, resource limits, ...). The \
        runtime `State` block, `NetworkSettings`, and `Mounts` are excluded — \
        use `sak docker info` for the full inspect payload.\n\n\
        Output is pretty-printed JSON. With --path, the same dot-notation or \
        JSON Pointer expression supported by `sak k8s get --path` is applied \
        to the subset and only the extracted value is emitted. Exit code 1 if \
        the container does not exist; exit code 2 on any other error.",
    after_help = "\
Examples:
  sak docker config web1                            Configuration subset as JSON
  sak docker config abc123                          Look up by short ID
  sak docker config web1 --path .Config.Image      Just the image reference
  sak docker config web1 --path .Config.Env        Environment variables
  sak docker config web1 --path .HostConfig.Binds  Bind mounts
  sak docker config web1 --path /HostConfig        JSON Pointer also works"
)]
pub struct ConfigArgs {
    /// Container name or ID (full or short)
    pub name: String,

    /// Extract a field via dot notation (`.Config`) or JSON Pointer
    /// (`/Config`)
    #[arg(long)]
    pub path: Option<String>,

    /// Maximum number of output lines
    #[arg(long)]
    pub limit: Option<usize>,
}

pub async fn run(args: &ConfigArgs) -> Result<ExitCode> {
    let client = DockerClient::connect()?;

    let path = format!("/containers/{}/json", args.name);

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

/// Project the configuration-relevant keys out of a container metadata
/// object. Missing keys are simply skipped.
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
            "Id": "abc123",
            "Name": "/web1",
            "State": {"Status": "running", "Pid": 4242},
            "Config": {
                "Image": "nginx:latest",
                "Env": ["FOO=bar"],
                "Cmd": ["nginx", "-g", "daemon off;"]
            },
            "HostConfig": {
                "Binds": ["/host:/container"],
                "RestartPolicy": {"Name": "always"}
            },
            "NetworkSettings": {"IPAddress": "172.17.0.2"},
            "Mounts": []
        });
        let subset = config_subset(&meta);
        let obj = subset.as_object().unwrap();
        assert_eq!(obj.len(), 2);
        assert!(obj.contains_key("Config"));
        assert!(obj.contains_key("HostConfig"));
        assert!(!obj.contains_key("State"));
        assert!(!obj.contains_key("NetworkSettings"));
        assert!(!obj.contains_key("Mounts"));
        assert!(!obj.contains_key("Id"));
    }

    #[test]
    fn config_subset_skips_missing_keys() {
        let meta = json!({
            "Id": "abc",
            "Config": {"Image": "alpine"}
        });
        let subset = config_subset(&meta);
        let obj = subset.as_object().unwrap();
        assert_eq!(obj.len(), 1);
        assert!(obj.contains_key("Config"));
        assert!(!obj.contains_key("HostConfig"));
    }

    #[test]
    fn config_subset_handles_non_object_input() {
        let subset = config_subset(&json!("oops"));
        assert!(subset.as_object().unwrap().is_empty());
    }
}
