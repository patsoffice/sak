//! `sak docker list` — list containers on the local Docker daemon.
//!
//! Issues a `GET /containers/json?all=true` against the discovered unix socket
//! via [`super::client::DockerClient::get_json`] and emits one line per
//! container:
//!
//! ```text
//! id<TAB>name<TAB>image<TAB>status<TAB>ports
//! ```
//!
//! Sorted by name for deterministic output. The `id` column is the short
//! 12-character form (matching `docker ps`); `name` strips the leading `/`
//! that the Engine API returns; `ports` is a comma-separated list in the same
//! `host:public->private/proto` shape `docker ps` uses, or `-` if the
//! container exposes no ports. `--format json` emits the raw container
//! metadata as newline-delimited JSON instead.

use std::io;
use std::process::ExitCode;

use anyhow::{Result, bail};
use clap::Args;
use serde_json::Value;

use crate::docker::client::DockerClient;
use crate::output::BoundedWriter;

#[derive(Args)]
#[command(
    about = "List Docker containers",
    long_about = "List containers on the local Docker daemon, including stopped \
        ones (the API is called with `all=true`).\n\n\
        Default output is `id<TAB>name<TAB>image<TAB>status<TAB>ports`, sorted \
        by name. The `id` column is the short 12-character form. The `name` \
        column strips the leading `/` that the Engine API returns. The \
        `ports` column is a comma-separated list of `host:public->private/proto` \
        bindings (or `private/proto` for unpublished ports), or a literal `-` \
        when the container exposes no ports.\n\n\
        `--format json` emits the raw container metadata one JSON object per \
        line (NDJSON), suitable for piping into `sak json query` or jq.",
    after_help = "\
Examples:
  sak docker list                    All containers (running and stopped)
  sak docker list --format json      NDJSON for further processing
  sak docker list --limit 20         Cap output at 20 containers"
)]
pub struct ListArgs {
    /// Output format
    #[arg(long, value_enum, default_value_t = Format::Tsv)]
    pub format: Format,

    /// Maximum number of output lines
    #[arg(long)]
    pub limit: Option<usize>,
}

#[derive(Clone, Copy, clap::ValueEnum)]
pub enum Format {
    /// Tab-separated columns: id, name, image, status, ports
    Tsv,
    /// Newline-delimited JSON, one container per line
    Json,
}

pub async fn run(args: &ListArgs) -> Result<ExitCode> {
    let client = DockerClient::connect()?;

    let path = "/containers/json?all=true";
    let body = match client.get_json(path).await? {
        Some(v) => v,
        None => return Ok(ExitCode::from(1)),
    };

    let Value::Array(mut items) = body else {
        bail!("Docker response for {path} was not an array");
    };

    items.sort_by_key(container_name);

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

/// Pull a stable display name out of a container JSON object.
///
/// The Engine API returns names as a `Names` array of `/`-prefixed strings.
/// We pick the first entry, strip the leading `/`, and fall back to `"-"` if
/// the field is missing or empty.
fn container_name(item: &Value) -> String {
    item.get("Names")
        .and_then(Value::as_array)
        .and_then(|arr| arr.first())
        .and_then(Value::as_str)
        .map(|n| n.strip_prefix('/').unwrap_or(n).to_string())
        .unwrap_or_else(|| "-".to_string())
}

fn format_row(item: &Value) -> String {
    let id = item
        .get("Id")
        .and_then(Value::as_str)
        .map(|s| s.chars().take(12).collect::<String>())
        .unwrap_or_else(|| "-".to_string());
    let name = container_name(item);
    let image = item
        .get("Image")
        .and_then(Value::as_str)
        .unwrap_or("-")
        .to_string();
    let status = item
        .get("Status")
        .and_then(Value::as_str)
        .unwrap_or("-")
        .to_string();
    let ports = format_ports(item);
    format!("{id}\t{name}\t{image}\t{status}\t{ports}")
}

/// Render the `Ports` array the way `docker ps` does.
///
/// Each entry is `{IP, PrivatePort, PublicPort?, Type}`. Published ports
/// (those with `PublicPort` set) render as `IP:PublicPort->PrivatePort/Type`;
/// unpublished ports render as `PrivatePort/Type`. The output is comma-joined
/// in the order the daemon returned, deduplicated to avoid the noise Docker's
/// own client filters out (the same port often appears once per binding IP).
/// Returns `"-"` when there are no ports.
fn format_ports(item: &Value) -> String {
    let Some(arr) = item.get("Ports").and_then(Value::as_array) else {
        return "-".to_string();
    };
    if arr.is_empty() {
        return "-".to_string();
    }
    let mut seen: Vec<String> = Vec::new();
    for p in arr {
        let private = p.get("PrivatePort").and_then(Value::as_u64);
        let proto = p.get("Type").and_then(Value::as_str).unwrap_or("tcp");
        let Some(private) = private else { continue };
        let rendered = match (
            p.get("PublicPort").and_then(Value::as_u64),
            p.get("IP").and_then(Value::as_str),
        ) {
            (Some(public), Some(ip)) if !ip.is_empty() => {
                format!("{ip}:{public}->{private}/{proto}")
            }
            (Some(public), _) => format!("{public}->{private}/{proto}"),
            _ => format!("{private}/{proto}"),
        };
        if !seen.contains(&rendered) {
            seen.push(rendered);
        }
    }
    if seen.is_empty() {
        "-".to_string()
    } else {
        seen.join(", ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn format_row_renders_running_container_with_published_port() {
        let item = json!({
            "Id": "abcdef0123456789aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "Names": ["/web1"],
            "Image": "nginx:latest",
            "Status": "Up 2 hours",
            "Ports": [
                {"IP": "0.0.0.0", "PrivatePort": 80, "PublicPort": 8080, "Type": "tcp"}
            ]
        });
        assert_eq!(
            format_row(&item),
            "abcdef012345\tweb1\tnginx:latest\tUp 2 hours\t0.0.0.0:8080->80/tcp"
        );
    }

    #[test]
    fn format_row_handles_stopped_container_with_no_ports() {
        let item = json!({
            "Id": "0123456789ab",
            "Names": ["/db"],
            "Image": "postgres:16",
            "Status": "Exited (0) 3 days ago",
            "Ports": []
        });
        assert_eq!(
            format_row(&item),
            "0123456789ab\tdb\tpostgres:16\tExited (0) 3 days ago\t-"
        );
    }

    #[test]
    fn format_row_renders_unpublished_port_without_arrow() {
        let item = json!({
            "Id": "deadbeefcafe1111",
            "Names": ["/api"],
            "Image": "myimage",
            "Status": "Up 1 minute",
            "Ports": [
                {"PrivatePort": 5432, "Type": "tcp"}
            ]
        });
        assert_eq!(
            format_row(&item),
            "deadbeefcafe\tapi\tmyimage\tUp 1 minute\t5432/tcp"
        );
    }

    #[test]
    fn format_ports_dedupes_repeated_bindings() {
        let item = json!({
            "Ports": [
                {"IP": "0.0.0.0", "PrivatePort": 80, "PublicPort": 8080, "Type": "tcp"},
                {"IP": "0.0.0.0", "PrivatePort": 80, "PublicPort": 8080, "Type": "tcp"},
                {"IP": "::", "PrivatePort": 80, "PublicPort": 8080, "Type": "tcp"}
            ]
        });
        assert_eq!(format_ports(&item), "0.0.0.0:8080->80/tcp, :::8080->80/tcp");
    }

    #[test]
    fn container_name_strips_leading_slash() {
        let item = json!({"Names": ["/foo", "/bar"]});
        assert_eq!(container_name(&item), "foo");
    }

    #[test]
    fn container_name_falls_back_to_dash() {
        let item = json!({});
        assert_eq!(container_name(&item), "-");
    }
}
