use std::process::ExitCode;

use anyhow::{Context, Result};
use clap::Args;
use serde_json::Value;

use crate::helm::client::{self, Conn};
use crate::helm::{Format, emit_to_stdout, render_cell};
use crate::output::BoundedWriter;

/// Fixed TSV column set, in emission order. Matches the snake_case keys
/// `helm history -o json` produces, so each column is a direct key lookup.
const COLUMNS: [&str; 6] = [
    "revision",
    "updated",
    "status",
    "chart",
    "app_version",
    "description",
];

#[derive(Args)]
#[command(
    about = "Show a Helm release's revision history as TSV (read-only)",
    long_about = "List a Helm release's revision history via `helm history <release> -o json` \
        and emit one TSV row per revision with the columns \
        revision, updated, status, chart, app_version, description.\n\n\
        Rows are ordered as `helm` returns them (oldest first; the highest \
        revision is the current one). `--max` caps the number of revisions \
        `helm` returns (most recent N). Use `--format json` for the raw `helm` \
        JSON array.\n\n\
        Cluster, auth, and namespace resolution are whatever `helm` itself \
        uses (`KUBECONFIG` / `~/.kube/config`).\n\n\
        Exit status: 0 when the release has history, 1 when it does not exist, \
        2 on any other error.",
    after_help = "\
Examples:
  sak helm history cilium -n kube-system           Full revision history as TSV
  sak helm history cilium -n kube-system --max 5    Most recent 5 revisions
  sak helm history cilium -n kube-system --format json"
)]
pub struct HistoryArgs {
    /// Release name whose history to show
    #[arg(value_name = "RELEASE")]
    pub release: String,

    /// Namespace of the release (default: the kubeconfig's current namespace)
    #[arg(long, short = 'n', value_name = "NS")]
    pub namespace: Option<String>,

    /// Show at most the N most recent revisions (forwarded to `helm --max`)
    #[arg(long, value_name = "N")]
    pub max: Option<u32>,

    /// Output format
    #[arg(long, value_enum, default_value_t = Format::Tsv)]
    pub format: Format,

    /// Maximum number of output lines (bounds output, not the helm fetch)
    #[arg(long)]
    pub limit: Option<usize>,
}

pub fn run(args: &HistoryArgs) -> Result<ExitCode> {
    let argv = build_argv(args);
    let argv_refs: Vec<&str> = argv.iter().map(String::as_str).collect();
    let conn = Conn {
        namespace: args.namespace.as_deref(),
        ..Default::default()
    };
    // A missing release is "no results" (exit 1); the chokepoint maps helm's
    // not-found stderr to Ok(None).
    let Some(stdout) = client::invoke_found("history", None, &argv_refs, conn)? else {
        return Ok(ExitCode::from(1));
    };
    emit_to_stdout(&stdout, args.format, args.limit, "[]", emit_tsv)
}

/// Assemble the `helm history` argv: the release name (positional), `-o json`,
/// and the optional `--max`. Connection flags come from `Conn`.
fn build_argv(args: &HistoryArgs) -> Vec<String> {
    let mut v = vec![
        args.release.clone(),
        "--output".to_string(),
        "json".to_string(),
    ];
    if let Some(max) = args.max {
        v.push("--max".to_string());
        v.push(max.to_string());
    }
    v
}

/// Project `helm history -o json`'s top-level array into rows. A non-array
/// value (or non-object elements) yields no rows; missing / null fields render
/// `-`. Pure over its input so it's testable on hand-built fixtures.
pub fn walk(value: &Value) -> Vec<[String; 6]> {
    let Some(arr) = value.as_array() else {
        return Vec::new();
    };
    arr.iter()
        .filter(|rec| rec.is_object())
        .map(|rec| COLUMNS.map(|col| render_cell(rec.get(col))))
        .collect()
}

/// Parse `helm`'s JSON array, project rows, and emit a header + TSV rows. The
/// header is decoration (not counted toward `--limit`); no revisions → no
/// results.
fn emit_tsv(writer: &mut BoundedWriter<'_>, stdout: &[u8]) -> Result<bool> {
    let text = String::from_utf8_lossy(stdout);
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Ok(false);
    }
    let value: Value =
        serde_json::from_str(trimmed).context("parsing `helm history -o json` output")?;
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

    fn bare() -> HistoryArgs {
        HistoryArgs {
            release: "cilium".to_string(),
            namespace: None,
            max: None,
            format: Format::Tsv,
            limit: None,
        }
    }

    #[test]
    fn default_argv_is_release_plus_json() {
        assert_eq!(build_argv(&bare()), vec!["cilium", "--output", "json"]);
    }

    #[test]
    fn max_appends_flag() {
        let mut args = bare();
        args.max = Some(5);
        assert_eq!(
            build_argv(&args),
            vec!["cilium", "--output", "json", "--max", "5"]
        );
    }

    #[test]
    fn walk_projects_columns_in_order() {
        let v = json!([
            {
                "revision": 16,
                "updated": "2026-05-01T03:14:56Z",
                "status": "superseded",
                "chart": "cilium-1.19.3",
                "app_version": "1.19.3",
                "description": "Upgrade complete"
            },
            {
                "revision": 17,
                "updated": "2026-05-15T03:58:47Z",
                "status": "deployed",
                "chart": "cilium-1.19.4",
                "app_version": "1.19.4",
                "description": "Upgrade complete"
            }
        ]);
        let rows = walk(&v);
        assert_eq!(rows.len(), 2);
        assert_eq!(
            rows[0],
            [
                "16", // numeric revision rendered as string
                "2026-05-01T03:14:56Z",
                "superseded",
                "cilium-1.19.3",
                "1.19.3",
                "Upgrade complete",
            ]
            .map(String::from)
        );
        assert_eq!(rows[1][2], "deployed");
    }

    #[test]
    fn walk_renders_missing_fields_as_dash() {
        let v = json!([{"revision": 1}]);
        let row = &walk(&v)[0];
        assert_eq!(row[0], "1");
        assert_eq!(row[1], "-"); // updated
        assert_eq!(row[5], "-"); // description
    }

    #[test]
    fn walk_non_array_yields_no_rows() {
        assert!(walk(&json!({"revision": 1})).is_empty());
        assert!(walk(&json!("nope")).is_empty());
    }

    #[test]
    fn walk_skips_non_object_elements() {
        let v = json!([{"revision": 1}, "garbage", 42, {"revision": 2}]);
        let rows = walk(&v);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0][0], "1");
        assert_eq!(rows[1][0], "2");
    }
}
