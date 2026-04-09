//! `sak lxc images` — list images on the local LXD/Incus daemon.
//!
//! Issues a `GET /1.0/images?recursion=1` against the discovered unix socket
//! via [`super::client::LxcClient::get_json`] and emits one line per image:
//!
//! ```text
//! fingerprint<TAB>aliases<TAB>arch<TAB>size<TAB>description
//! ```
//!
//! Sorted by fingerprint for deterministic output. `aliases` is a
//! comma-separated list of alias names, or `-` if the image has none. `size`
//! is the raw byte count from the LXD response. `description` comes from
//! `properties.description`, falling back to `-` if absent.

use std::io;
use std::process::ExitCode;

use anyhow::{Result, bail};
use clap::Args;
use serde_json::Value;

use crate::lxc::client::LxcClient;
use crate::output::BoundedWriter;

#[derive(Args)]
#[command(
    about = "List images on the local LXD/Incus daemon",
    long_about = "List images stored on the local LXD or Incus daemon.\n\n\
        Default output is `fingerprint<TAB>aliases<TAB>arch<TAB>size<TAB>description`, \
        sorted by fingerprint. The `aliases` column is a comma-separated list \
        of alias names (e.g. `ubuntu/22.04,jammy`); a literal `-` is shown when \
        an image has no aliases. `size` is raw bytes. `description` is taken \
        from the image's `properties.description` field.",
    after_help = "\
Examples:
  sak lxc images                       Images in the default project
  sak lxc images --project mylab       Images in a specific LXD project
  sak lxc images --limit 20            Cap output at 20 images"
)]
pub struct ImagesArgs {
    /// LXD project to list images from (default: `default`)
    #[arg(long)]
    pub project: Option<String>,

    /// Maximum number of output lines
    #[arg(long)]
    pub limit: Option<usize>,
}

pub async fn run(args: &ImagesArgs) -> Result<ExitCode> {
    let client = LxcClient::connect()?;

    let path = match &args.project {
        Some(p) => format!("/1.0/images?project={p}"),
        None => "/1.0/images".to_string(),
    };

    let metadata = match client.get_json_recursive(&path, 1).await? {
        Some(v) => v,
        None => return Ok(ExitCode::from(1)),
    };

    let Value::Array(mut items) = metadata else {
        bail!("LXD response for {path} was not an array");
    };

    items.sort_by(|a, b| {
        let af = a.get("fingerprint").and_then(Value::as_str).unwrap_or("");
        let bf = b.get("fingerprint").and_then(Value::as_str).unwrap_or("");
        af.cmp(bf)
    });

    let stdout = io::stdout();
    let handle = stdout.lock();
    let mut writer = BoundedWriter::new(handle, args.limit);

    let mut wrote_any = false;
    for item in &items {
        if !writer.write_line(&format_row(item))? {
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
    let fingerprint = item
        .get("fingerprint")
        .and_then(Value::as_str)
        .unwrap_or("-");
    let aliases = format_aliases(item);
    let arch = item
        .get("architecture")
        .and_then(Value::as_str)
        .unwrap_or("-");
    let size = item
        .get("size")
        .and_then(Value::as_i64)
        .map(|n| n.to_string())
        .unwrap_or_else(|| "-".to_string());
    let description = item
        .get("properties")
        .and_then(|p| p.get("description"))
        .and_then(Value::as_str)
        .unwrap_or("-");
    format!("{fingerprint}\t{aliases}\t{arch}\t{size}\t{description}")
}

/// Join the alias `name` fields with commas. Returns `-` when the array is
/// missing or empty so the column is never blank.
fn format_aliases(item: &Value) -> String {
    let Some(arr) = item.get("aliases").and_then(Value::as_array) else {
        return "-".to_string();
    };
    let names: Vec<&str> = arr
        .iter()
        .filter_map(|a| a.get("name").and_then(Value::as_str))
        .collect();
    if names.is_empty() {
        "-".to_string()
    } else {
        names.join(",")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn format_row_full_image() {
        let item = json!({
            "fingerprint": "abc123def456",
            "aliases": [
                {"name": "ubuntu/22.04", "description": ""},
                {"name": "jammy", "description": ""}
            ],
            "architecture": "x86_64",
            "size": 123456789,
            "properties": {
                "description": "Ubuntu 22.04 LTS",
                "os": "ubuntu"
            }
        });
        assert_eq!(
            format_row(&item),
            "abc123def456\tubuntu/22.04,jammy\tx86_64\t123456789\tUbuntu 22.04 LTS"
        );
    }

    #[test]
    fn format_row_no_aliases_or_description() {
        let item = json!({
            "fingerprint": "deadbeef",
            "aliases": [],
            "architecture": "aarch64",
            "size": 42
        });
        assert_eq!(format_row(&item), "deadbeef\t-\taarch64\t42\t-");
    }

    #[test]
    fn format_row_handles_missing_fields() {
        let item = json!({});
        assert_eq!(format_row(&item), "-\t-\t-\t-\t-");
    }
}
