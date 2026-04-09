//! `sak docker images` — list images on the local Docker daemon.
//!
//! Issues a `GET /images/json` against the discovered unix socket via
//! [`super::client::DockerClient::get_json`] and emits one line per repo:tag:
//!
//! ```text
//! id<TAB>repo<TAB>tag<TAB>size<TAB>created
//! ```
//!
//! Sorted by id for deterministic output. The `id` column is the short
//! 12-character form (matching `docker images`), with the `sha256:` prefix
//! stripped. An image with multiple `RepoTags` produces one row per tag, the
//! way `docker images` does. Untagged images (`RepoTags` missing or
//! containing `<none>:<none>`) render with `repo` and `tag` set to `<none>`.
//! `size` is the raw byte count from the Engine API; `created` is the raw
//! unix timestamp. `--format json` emits the raw image metadata as
//! newline-delimited JSON instead.

use std::io;
use std::process::ExitCode;

use anyhow::{Result, bail};
use clap::Args;
use serde_json::Value;

use crate::docker::client::DockerClient;
use crate::output::BoundedWriter;

#[derive(Args)]
#[command(
    about = "List Docker images",
    long_about = "List images on the local Docker daemon.\n\n\
        Default output is `id<TAB>repo<TAB>tag<TAB>size<TAB>created`, sorted \
        by id. The `id` column is the short 12-character form with the \
        `sha256:` prefix stripped. An image with multiple repo tags produces \
        one row per tag (matching `docker images`). Untagged images render \
        with `repo` and `tag` both set to `<none>`. `size` is raw bytes; \
        `created` is the raw unix timestamp.\n\n\
        `--format json` emits the raw image metadata one JSON object per \
        line (NDJSON), suitable for piping into `sak json query` or jq.",
    after_help = "\
Examples:
  sak docker images                    All images on the daemon
  sak docker images --format json      NDJSON for further processing
  sak docker images --limit 20         Cap output at 20 rows"
)]
pub struct ImagesArgs {
    /// Output format
    #[arg(long, value_enum, default_value_t = Format::Tsv)]
    pub format: Format,

    /// Maximum number of output lines
    #[arg(long)]
    pub limit: Option<usize>,
}

#[derive(Clone, Copy, clap::ValueEnum)]
pub enum Format {
    /// Tab-separated columns: id, repo, tag, size, created
    Tsv,
    /// Newline-delimited JSON, one image per line
    Json,
}

pub async fn run(args: &ImagesArgs) -> Result<ExitCode> {
    let client = DockerClient::connect()?;

    let path = "/images/json";
    let body = match client.get_json(path).await? {
        Some(v) => v,
        None => return Ok(ExitCode::from(1)),
    };

    let Value::Array(mut items) = body else {
        bail!("Docker response for {path} was not an array");
    };

    items.sort_by_key(short_id);

    let stdout = io::stdout();
    let handle = stdout.lock();
    let mut writer = BoundedWriter::new(handle, args.limit);

    let mut wrote_any = false;
    for item in &items {
        match args.format {
            Format::Tsv => {
                for line in format_rows(item) {
                    if !writer.write_line(&line)? {
                        writer.flush()?;
                        return Ok(if wrote_any {
                            ExitCode::SUCCESS
                        } else {
                            ExitCode::from(1)
                        });
                    }
                    wrote_any = true;
                }
            }
            Format::Json => {
                let line = serde_json::to_string(item)?;
                if !writer.write_line(&line)? {
                    break;
                }
                wrote_any = true;
            }
        }
    }

    writer.flush()?;
    if wrote_any {
        Ok(ExitCode::SUCCESS)
    } else {
        Ok(ExitCode::from(1))
    }
}

/// Short, 12-character image id with the `sha256:` prefix stripped, matching
/// `docker images`. Falls back to `"-"` when the field is missing.
fn short_id(item: &Value) -> String {
    item.get("Id")
        .and_then(Value::as_str)
        .map(|s| s.strip_prefix("sha256:").unwrap_or(s))
        .map(|s| s.chars().take(12).collect::<String>())
        .unwrap_or_else(|| "-".to_string())
}

/// Render one image into one row per `RepoTags` entry.
///
/// An image with multiple repo tags becomes multiple rows (matching
/// `docker images`). Untagged images render with `repo` and `tag` set to
/// `<none>`, again matching the docker CLI.
fn format_rows(item: &Value) -> Vec<String> {
    let id = short_id(item);
    let size = item
        .get("Size")
        .and_then(Value::as_i64)
        .map(|n| n.to_string())
        .unwrap_or_else(|| "-".to_string());
    let created = item
        .get("Created")
        .and_then(Value::as_i64)
        .map(|n| n.to_string())
        .unwrap_or_else(|| "-".to_string());

    let tags: Vec<&str> = item
        .get("RepoTags")
        .and_then(Value::as_array)
        .map(|arr| arr.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();

    if tags.is_empty() {
        return vec![format!("{id}\t<none>\t<none>\t{size}\t{created}")];
    }

    tags.into_iter()
        .map(|t| {
            let (repo, tag) = split_repo_tag(t);
            format!("{id}\t{repo}\t{tag}\t{size}\t{created}")
        })
        .collect()
}

/// Split a `repo:tag` string into its two halves.
///
/// The repo half may contain colons (registry host with port, e.g.
/// `registry.example.com:5000/foo`), so we split on the *last* colon. If
/// there is no colon at all, the whole string is the repo and the tag is
/// `<none>`.
fn split_repo_tag(s: &str) -> (&str, &str) {
    match s.rsplit_once(':') {
        Some((repo, tag)) => (repo, tag),
        None => (s, "<none>"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn format_rows_single_tag() {
        let item = json!({
            "Id": "sha256:abcdef0123456789aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "RepoTags": ["nginx:latest"],
            "Size": 142_000_000,
            "Created": 1_700_000_000_i64
        });
        assert_eq!(
            format_rows(&item),
            vec!["abcdef012345\tnginx\tlatest\t142000000\t1700000000".to_string()]
        );
    }

    #[test]
    fn format_rows_multiple_tags_become_multiple_rows() {
        let item = json!({
            "Id": "sha256:0123456789abcdef0000000000000000000000000000000000000000000000",
            "RepoTags": ["myapp:1.2.3", "myapp:latest"],
            "Size": 1024,
            "Created": 1_600_000_000_i64
        });
        assert_eq!(
            format_rows(&item),
            vec![
                "0123456789ab\tmyapp\t1.2.3\t1024\t1600000000".to_string(),
                "0123456789ab\tmyapp\tlatest\t1024\t1600000000".to_string(),
            ]
        );
    }

    #[test]
    fn format_rows_untagged_image_uses_none_placeholders() {
        let item = json!({
            "Id": "sha256:deadbeefcafe1111000000000000000000000000000000000000000000000000",
            "RepoTags": null,
            "Size": 0,
            "Created": 0
        });
        assert_eq!(
            format_rows(&item),
            vec!["deadbeefcafe\t<none>\t<none>\t0\t0".to_string()]
        );
    }

    #[test]
    fn format_rows_explicit_none_none_tag_treated_like_untagged() {
        // Docker reports dangling images with RepoTags = ["<none>:<none>"];
        // we split on the last colon so the row still reads <none>/<none>.
        let item = json!({
            "Id": "sha256:1111111111110000000000000000000000000000000000000000000000000000",
            "RepoTags": ["<none>:<none>"],
            "Size": 5,
            "Created": 1
        });
        assert_eq!(
            format_rows(&item),
            vec!["111111111111\t<none>\t<none>\t5\t1".to_string()]
        );
    }

    #[test]
    fn split_repo_tag_handles_registry_with_port() {
        // The repo half contains its own colon — we must split on the last
        // colon, not the first, or the registry port lands in the tag column.
        assert_eq!(
            split_repo_tag("registry.example.com:5000/team/app:v2"),
            ("registry.example.com:5000/team/app", "v2")
        );
    }

    #[test]
    fn split_repo_tag_no_colon_means_no_tag() {
        assert_eq!(split_repo_tag("bareword"), ("bareword", "<none>"));
    }

    #[test]
    fn short_id_strips_sha256_prefix_and_truncates() {
        let item = json!({
            "Id": "sha256:abcdef0123456789aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
        });
        assert_eq!(short_id(&item), "abcdef012345");
    }

    #[test]
    fn short_id_falls_back_to_dash_when_missing() {
        assert_eq!(short_id(&json!({})), "-");
    }
}
