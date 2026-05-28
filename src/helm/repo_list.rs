use crate::output::Outcome;

use anyhow::{Context, Result};
use clap::Args;
use serde_json::Value;

use crate::helm::client::{self, Conn};
use crate::helm::{Format, emit_to_stdout, render_cell};
use crate::output::BoundedWriter;

/// Fixed TSV column set, in emission order. `name` / `url` are direct keys
/// from `helm repo list -o json`; `oci` is derived from the URL scheme.
const COLUMNS: [&str; 3] = ["name", "url", "oci"];

#[derive(Args)]
#[command(
    about = "List configured Helm chart repositories as TSV (read-only)",
    long_about = "List the chart repositories configured in helm's repositories.yaml \
        (`~/.config/helm/repositories.yaml`) via `helm repo list -o json`, one \
        TSV row per repo with the columns name, url, oci.\n\n\
        `oci` is `true` for OCI registries (URLs starting with `oci://`) and \
        `false` for traditional HTTP chart repos. This reads helm's local \
        config only — it does not contact any registry or cluster. Use \
        `--format json` for the raw `helm` JSON array.\n\n\
        Exit status: 0 when at least one repo is configured, 1 when none are, \
        2 on error.",
    after_help = "\
Examples:
  sak helm repo-list                Configured chart repositories as TSV
  sak helm repo-list --format json  Raw helm JSON array"
)]
pub struct RepoListArgs {
    /// Output format
    #[arg(long, value_enum, default_value_t = Format::Tsv)]
    pub format: Format,

    /// Maximum number of output lines (bounds output, not the helm fetch)
    #[arg(long)]
    pub limit: Option<usize>,
}

pub fn run(args: &RepoListArgs) -> Result<Outcome> {
    // `helm repo list` reads the local repositories.yaml — no cluster or
    // namespace involved, so the connection is the ambient default.
    let stdout = client::invoke_ok("repo", Some("list"), &["--output", "json"], Conn::default())?;
    emit_to_stdout(&stdout, args.format, args.limit, "[]", emit_tsv)
}

/// Project `helm repo list -o json`'s top-level array into rows. A non-array
/// value (or non-object elements) yields no rows; `oci` is `true` when the
/// `url` uses the `oci://` scheme. Pure over its input so it's testable on
/// hand-built fixtures.
pub fn walk(value: &Value) -> Vec<[String; 3]> {
    let Some(arr) = value.as_array() else {
        return Vec::new();
    };
    arr.iter()
        .filter(|rec| rec.is_object())
        .map(|rec| {
            let oci = rec
                .get("url")
                .and_then(Value::as_str)
                .is_some_and(|u| u.starts_with("oci://"));
            [
                render_cell(rec.get("name")),
                render_cell(rec.get("url")),
                oci.to_string(),
            ]
        })
        .collect()
}

/// Parse `helm`'s JSON array, project rows, and emit a header + TSV rows. The
/// header is decoration (not counted toward `--limit`); no repos → no results.
fn emit_tsv(writer: &mut BoundedWriter<'_>, stdout: &[u8]) -> Result<bool> {
    let text = String::from_utf8_lossy(stdout);
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Ok(false);
    }
    let value: Value =
        serde_json::from_str(trimmed).context("parsing `helm repo list -o json` output")?;
    let rows = walk(&value);
    if rows.is_empty() {
        return Ok(false);
    }
    writer.write_decoration(&COLUMNS.join("\t"))?;
    for row in &rows {
        if !writer.write_line(&row.join("\t"))? {
            break;
        }
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn walk_projects_name_url_and_detects_oci() {
        let v = json!([
            {"name": "bitnami", "url": "https://charts.bitnami.com/bitnami"},
            {"name": "myoci", "url": "oci://registry.example.com/charts"}
        ]);
        let rows = walk(&v);
        assert_eq!(rows.len(), 2);
        assert_eq!(
            rows[0],
            ["bitnami", "https://charts.bitnami.com/bitnami", "false"].map(String::from)
        );
        assert_eq!(
            rows[1],
            ["myoci", "oci://registry.example.com/charts", "true"].map(String::from)
        );
    }

    #[test]
    fn walk_renders_missing_url_as_dash_and_not_oci() {
        let v = json!([{"name": "broken"}]);
        let row = &walk(&v)[0];
        assert_eq!(row[0], "broken");
        assert_eq!(row[1], "-"); // url missing
        assert_eq!(row[2], "false"); // not oci
    }

    #[test]
    fn walk_oci_match_is_scheme_anchored() {
        // A URL that merely contains "oci" is not an OCI registry.
        let v = json!([{"name": "x", "url": "https://oci.example.com/charts"}]);
        assert_eq!(walk(&v)[0][2], "false");
    }

    #[test]
    fn walk_non_array_yields_no_rows() {
        assert!(walk(&json!({"name": "x"})).is_empty());
        assert!(walk(&json!("nope")).is_empty());
    }

    #[test]
    fn walk_skips_non_object_elements() {
        let v = json!([{"name": "a"}, "garbage", 42, {"name": "b"}]);
        let rows = walk(&v);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0][0], "a");
        assert_eq!(rows[1][0], "b");
    }
}
