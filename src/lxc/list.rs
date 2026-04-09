//! `sak lxc list` — list LXD/Incus instances on the local daemon.
//!
//! Issues a `GET /1.0/instances?recursion=2` against the discovered unix
//! socket via [`super::client::LxcClient::get_json_recursive`] and emits one
//! line per instance:
//!
//! ```text
//! name<TAB>type<TAB>status<TAB>ipv4<TAB>ipv6
//! ```
//!
//! Sorted by name for deterministic output. `ipv4` / `ipv6` are the first
//! global-scope address found on any interface, or `-` if none. `--format json`
//! emits the raw instance metadata as newline-delimited JSON instead.

use std::io;
use std::process::ExitCode;

use anyhow::{Result, bail};
use clap::Args;
use serde_json::Value;

use crate::lxc::client::LxcClient;
use crate::output::BoundedWriter;

#[derive(Args)]
#[command(
    about = "List LXD/Incus instances",
    long_about = "List instances on the local LXD or Incus daemon.\n\n\
        Default output is `name<TAB>type<TAB>status<TAB>ipv4<TAB>ipv6`, sorted \
        by name. The ipv4 and ipv6 columns show the first global-scope address \
        found on any interface (loopback and link-local are skipped); a literal \
        `-` is shown when no global address is configured.\n\n\
        `--format json` emits the raw instance metadata one JSON object per \
        line (NDJSON), suitable for piping into `sak json query` or jq.",
    after_help = "\
Examples:
  sak lxc list                       Instances in the default project
  sak lxc list --project mylab       Instances in a specific LXD project
  sak lxc list --format json         NDJSON for further processing
  sak lxc list --limit 20            Cap output at 20 instances"
)]
pub struct ListArgs {
    /// LXD project to list instances from (default: `default`)
    #[arg(long)]
    pub project: Option<String>,

    /// Output format
    #[arg(long, value_enum, default_value_t = Format::Tsv)]
    pub format: Format,

    /// Maximum number of output lines
    #[arg(long)]
    pub limit: Option<usize>,
}

#[derive(Clone, Copy, clap::ValueEnum)]
pub enum Format {
    /// Tab-separated columns: name, type, status, ipv4, ipv6
    Tsv,
    /// Newline-delimited JSON, one instance per line
    Json,
}

pub async fn run(args: &ListArgs) -> Result<ExitCode> {
    let client = LxcClient::connect()?;

    let path = match &args.project {
        Some(p) => format!("/1.0/instances?project={p}"),
        None => "/1.0/instances".to_string(),
    };

    let metadata = match client.get_json_recursive(&path, 2).await? {
        Some(v) => v,
        None => return Ok(ExitCode::from(1)),
    };

    let Value::Array(mut items) = metadata else {
        bail!("LXD response for {path} was not an array");
    };

    items.sort_by(|a, b| {
        let an = a.get("name").and_then(Value::as_str).unwrap_or("");
        let bn = b.get("name").and_then(Value::as_str).unwrap_or("");
        an.cmp(bn)
    });

    let stdout = io::stdout();
    let handle = stdout.lock();
    let mut writer = BoundedWriter::new(handle, args.limit);

    let mut wrote_any = false;
    for item in &items {
        let line = match args.format {
            Format::Tsv => format_row(item),
            Format::Json => serde_json::to_string(item)?,
        };
        if !writer.write_line(&line)? {
            break;
        }
        wrote_any = true;
    }

    writer.flush()?;
    if wrote_any {
        Ok(ExitCode::SUCCESS)
    } else {
        Ok(ExitCode::from(1))
    }
}

fn format_row(item: &Value) -> String {
    let name = item.get("name").and_then(Value::as_str).unwrap_or("-");
    let kind = item.get("type").and_then(Value::as_str).unwrap_or("-");
    let status = item.get("status").and_then(Value::as_str).unwrap_or("-");
    let ipv4 = first_global_address(item, "inet").unwrap_or_else(|| "-".to_string());
    let ipv6 = first_global_address(item, "inet6").unwrap_or_else(|| "-".to_string());
    format!("{name}\t{kind}\t{status}\t{ipv4}\t{ipv6}")
}

/// Walk `state.network.<iface>.addresses[*]` and return the first address
/// whose `family` matches and whose `scope == "global"`. Interfaces are
/// visited in the order LXD returns them in the JSON object, which is stable
/// for a given snapshot.
fn first_global_address(item: &Value, family: &str) -> Option<String> {
    let network = item.get("state")?.get("network")?.as_object()?;
    for (_iface, info) in network {
        let addresses = info.get("addresses")?.as_array()?;
        for addr in addresses {
            let fam = addr.get("family").and_then(Value::as_str)?;
            let scope = addr.get("scope").and_then(Value::as_str)?;
            if fam == family
                && scope == "global"
                && let Some(a) = addr.get("address").and_then(Value::as_str)
            {
                return Some(a.to_string());
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn format_row_picks_first_global_addresses() {
        let item = json!({
            "name": "web1",
            "type": "container",
            "status": "Running",
            "state": {
                "network": {
                    "lo": {
                        "addresses": [
                            {"family": "inet", "address": "127.0.0.1", "scope": "local"}
                        ]
                    },
                    "eth0": {
                        "addresses": [
                            {"family": "inet", "address": "10.0.0.5", "scope": "global"},
                            {"family": "inet6", "address": "fe80::1", "scope": "link"},
                            {"family": "inet6", "address": "fd00::1", "scope": "global"}
                        ]
                    }
                }
            }
        });
        assert_eq!(
            format_row(&item),
            "web1\tcontainer\tRunning\t10.0.0.5\tfd00::1"
        );
    }

    #[test]
    fn format_row_handles_missing_state() {
        let item = json!({
            "name": "stopped",
            "type": "virtual-machine",
            "status": "Stopped"
        });
        assert_eq!(format_row(&item), "stopped\tvirtual-machine\tStopped\t-\t-");
    }

    #[test]
    fn format_row_handles_no_global_addresses() {
        let item = json!({
            "name": "iso",
            "type": "container",
            "status": "Running",
            "state": {
                "network": {
                    "lo": {
                        "addresses": [
                            {"family": "inet", "address": "127.0.0.1", "scope": "local"}
                        ]
                    }
                }
            }
        });
        assert_eq!(format_row(&item), "iso\tcontainer\tRunning\t-\t-");
    }
}
