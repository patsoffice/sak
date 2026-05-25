use std::process::ExitCode;

use anyhow::{Context, Result};
use clap::{Args, ValueEnum};
use serde_json::Value;

use crate::helm::client::{self, Conn};
use crate::helm::{Format, emit_to_stdout, render_cell};
use crate::output::BoundedWriter;

/// Where to search: the locally configured repos or the public Artifact Hub.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum Source {
    /// Configured chart repositories (`helm search repo`)
    Repo,
    /// Artifact Hub (`helm search hub`) — hits the network
    Hub,
}

impl Source {
    fn as_helm(self) -> &'static str {
        match self {
            Source::Repo => "repo",
            Source::Hub => "hub",
        }
    }

    /// TSV column set, which differs by source: hub results carry a chart `url`
    /// and put the chart name under `repository.name`.
    fn columns(self) -> &'static [&'static str] {
        match self {
            Source::Repo => &["name", "chart_version", "app_version", "description"],
            Source::Hub => &["url", "name", "chart_version", "app_version", "description"],
        }
    }
}

#[derive(Args)]
#[command(
    about = "Search charts in configured repos or on Artifact Hub as TSV (read-only)",
    long_about = "Search for charts via `helm search repo <term>` (the locally configured \
        repositories) or `helm search hub <term>` (the public Artifact Hub) \
        and emit one TSV row per result.\n\n\
        Columns differ by source: `repo` yields \
        name, chart_version, app_version, description; `hub` yields \
        url, name, chart_version, app_version, description (the chart name \
        comes from the hub result's repository). `--regexp` treats `<term>` as \
        a regular expression; `--versions` includes every chart version (repo \
        only). Use `--format json` for the raw `helm` JSON array.\n\n\
        `--source hub` contacts the network; `--source repo` reads the local \
        repo cache and errors (exit 2) if no repositories are configured.\n\n\
        Exit status: 0 when there are matches, 1 when there are none, 2 on error.",
    after_help = "\
Examples:
  sak helm search nginx                         Search configured repos
  sak helm search nginx --source hub            Search Artifact Hub
  sak helm search '^ingress' --regexp           Regex search of configured repos
  sak helm search redis --versions              Include all chart versions
  sak helm search nginx --source hub --format json"
)]
pub struct SearchArgs {
    /// Search term (a keyword, or a regex with `--regexp`)
    #[arg(value_name = "TERM")]
    pub term: String,

    /// Where to search
    #[arg(long, value_enum, default_value_t = Source::Repo)]
    pub source: Source,

    /// Treat the term as a regular expression (forwarded to `helm --regexp`)
    #[arg(long)]
    pub regexp: bool,

    /// Include all chart versions, not just the latest (repo only; `--versions`)
    #[arg(long)]
    pub versions: bool,

    /// Output format
    #[arg(long, value_enum, default_value_t = Format::Tsv)]
    pub format: Format,

    /// Maximum number of output lines (bounds output, not the helm fetch)
    #[arg(long)]
    pub limit: Option<usize>,
}

pub fn run(args: &SearchArgs) -> Result<ExitCode> {
    let argv = build_argv(args);
    let argv_refs: Vec<&str> = argv.iter().map(String::as_str).collect();
    // No cluster involved — repo searches the local cache, hub hits the
    // network. A repo search with no repositories configured is a hard error
    // (exit 2), distinct from an empty result set (exit 1).
    let stdout = client::invoke_ok("search", None, &argv_refs, Conn::default())?;
    let source = args.source;
    emit_to_stdout(&stdout, args.format, args.limit, "[]", move |w, out| {
        emit_tsv(w, out, source)
    })
}

/// Assemble the `helm search <repo|hub> <term>` argv plus flags.
fn build_argv(args: &SearchArgs) -> Vec<String> {
    let mut v = vec![
        args.source.as_helm().to_string(),
        args.term.clone(),
        "--output".to_string(),
        "json".to_string(),
    ];
    if args.regexp {
        v.push("--regexp".to_string());
    }
    if args.versions {
        v.push("--versions".to_string());
    }
    v
}

/// Project `helm search`'s JSON array into rows for the given source. A
/// non-array value (or non-object elements) yields no rows; missing fields
/// render `-`. Pure over its input so it's testable on hand-built fixtures.
pub fn walk(value: &Value, source: Source) -> Vec<Vec<String>> {
    let Some(arr) = value.as_array() else {
        return Vec::new();
    };
    arr.iter()
        .filter(|rec| rec.is_object())
        .map(|rec| match source {
            Source::Repo => vec![
                render_cell(rec.get("name")),
                render_cell(rec.get("version")),
                render_cell(rec.get("app_version")),
                render_cell(rec.get("description")),
            ],
            Source::Hub => vec![
                render_cell(rec.get("url")),
                render_cell(rec.get("repository").and_then(|r| r.get("name"))),
                render_cell(rec.get("version")),
                render_cell(rec.get("app_version")),
                render_cell(rec.get("description")),
            ],
        })
        .collect()
}

fn emit_tsv(writer: &mut BoundedWriter<'_>, stdout: &[u8], source: Source) -> Result<bool> {
    let text = String::from_utf8_lossy(stdout);
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Ok(false);
    }
    let value: Value =
        serde_json::from_str(trimmed).context("parsing `helm search -o json` output")?;
    let rows = walk(&value, source);
    if rows.is_empty() {
        return Ok(false);
    }
    writer.write_decoration(&source.columns().join("\t"))?;
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

    fn bare() -> SearchArgs {
        SearchArgs {
            term: "nginx".into(),
            source: Source::Repo,
            regexp: false,
            versions: false,
            format: Format::Tsv,
            limit: None,
        }
    }

    #[test]
    fn default_argv_searches_repo_with_json() {
        assert_eq!(
            build_argv(&bare()),
            vec!["repo", "nginx", "--output", "json"]
        );
    }

    #[test]
    fn hub_source_and_flags() {
        let mut args = bare();
        args.source = Source::Hub;
        args.regexp = true;
        args.versions = true;
        assert_eq!(
            build_argv(&args),
            vec!["hub", "nginx", "--output", "json", "--regexp", "--versions"]
        );
    }

    #[test]
    fn walk_repo_projects_name_version_appversion_description() {
        let v = json!([
            {"name": "bitnami/nginx", "version": "15.1.0", "app_version": "1.27.0", "description": "NGINX"}
        ]);
        let rows = walk(&v, Source::Repo);
        assert_eq!(
            rows[0],
            ["bitnami/nginx", "15.1.0", "1.27.0", "NGINX"].map(String::from)
        );
    }

    #[test]
    fn walk_hub_pulls_name_from_repository() {
        // Hub entries have no top-level name — it lives under `repository`.
        let v = json!([
            {
                "url": "https://artifacthub.io/packages/helm/cloudpirates-nginx/nginx",
                "version": "0.12.1",
                "app_version": "1.31.0",
                "description": "Nginx server.",
                "repository": {"url": "oci://registry-1.docker.io/cloudpirates/nginx", "name": "cloudpirates-nginx"}
            }
        ]);
        let rows = walk(&v, Source::Hub);
        assert_eq!(
            rows[0],
            [
                "https://artifacthub.io/packages/helm/cloudpirates-nginx/nginx",
                "cloudpirates-nginx",
                "0.12.1",
                "1.31.0",
                "Nginx server.",
            ]
            .map(String::from)
        );
    }

    #[test]
    fn walk_hub_missing_repository_renders_dash_name() {
        let v = json!([{"url": "https://x", "version": "1.0"}]);
        let rows = walk(&v, Source::Hub);
        assert_eq!(rows[0][0], "https://x");
        assert_eq!(rows[0][1], "-"); // name (no repository)
    }

    #[test]
    fn walk_non_array_yields_no_rows() {
        assert!(walk(&json!({"name": "x"}), Source::Repo).is_empty());
        assert!(walk(&json!("nope"), Source::Hub).is_empty());
    }

    #[test]
    fn columns_differ_by_source() {
        assert_eq!(
            Source::Repo.columns(),
            ["name", "chart_version", "app_version", "description"]
        );
        assert_eq!(
            Source::Hub.columns(),
            ["url", "name", "chart_version", "app_version", "description"]
        );
    }
}
