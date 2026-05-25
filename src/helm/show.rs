use std::process::ExitCode;

use anyhow::Result;
use clap::{Args, ValueEnum};

use crate::helm::client::{self, Conn};
use crate::helm::emit_text_to_stdout;

/// Which slice of a chart `helm show` should dump. These are `helm show`'s
/// positional subcommands (`helm show values <chart>`), forwarded as the first
/// argument; the chokepoint's `show` family is read-only, so each is safe.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum What {
    /// Everything: chart metadata, default values, and README concatenated
    All,
    /// The chart's `Chart.yaml` metadata (YAML)
    Chart,
    /// The chart's default `values.yaml` (YAML)
    Values,
    /// The chart's README (markdown)
    Readme,
    /// The chart's CRDs (YAML)
    Crds,
}

impl What {
    /// The `helm show` subcommand word.
    fn as_helm(self) -> &'static str {
        match self {
            What::All => "all",
            What::Chart => "chart",
            What::Values => "values",
            What::Readme => "readme",
            What::Crds => "crds",
        }
    }
}

#[derive(Args)]
#[command(
    about = "Show a chart's metadata / values / readme / crds without installing",
    long_about = "Inspect a chart via `helm show <what> <chart>` without installing it. The \
        chart can be a repo reference (`repo/chart`), an `oci://` ref, a local \
        directory, or a packaged `.tgz`.\n\n\
        `--what` selects the slice (default `all`): `chart` is the `Chart.yaml` \
        metadata, `values` is the default `values.yaml`, `readme` is the \
        chart's README (markdown), `crds` is its CRD manifests, and `all` \
        concatenates metadata + values + readme. Output is helm's native text \
        (YAML for chart/values/crds, markdown for readme) forwarded verbatim — \
        pipe `--what values` through `sak config query` for structured access. \
        `--version` picks a specific chart version; `--repo` names a chart repo \
        URL to pull from.\n\n\
        Exit status: 0 when output is produced, 1 when the slice is empty (e.g. \
        a chart with no CRDs or README), 2 on error (e.g. an unresolvable chart).",
    after_help = "\
Examples:
  sak helm show ./mychart                              Metadata + values + readme
  sak helm show ./mychart --what values                Default values.yaml
  sak helm show bitnami/nginx --what chart --repo https://charts.bitnami.com/bitnami
  sak helm show ./mychart --what values | sak config query .image.tag --format yaml"
)]
pub struct ShowArgs {
    /// Chart reference: `repo/chart`, an `oci://` ref, a local dir, or a `.tgz`
    #[arg(value_name = "CHART")]
    pub chart: String,

    /// Which slice of the chart to show
    #[arg(long, value_enum, default_value_t = What::All)]
    pub what: What,

    /// Chart version to show (default: latest)
    #[arg(long, value_name = "VER")]
    pub version: Option<String>,

    /// Chart repository URL to pull from (forwarded to `helm --repo`)
    #[arg(long, value_name = "URL")]
    pub repo: Option<String>,

    /// Maximum number of output lines
    #[arg(long)]
    pub limit: Option<usize>,
}

pub fn run(args: &ShowArgs) -> Result<ExitCode> {
    let argv = build_argv(args);
    let argv_refs: Vec<&str> = argv.iter().map(String::as_str).collect();
    // `helm show` resolves a chart, not a cluster release — no namespace. A
    // chart that can't be resolved is a hard error (exit 2), not "no results".
    let stdout = client::invoke_ok("show", None, &argv_refs, Conn::default())?;
    emit_text_to_stdout(&stdout, args.limit)
}

/// Assemble the `helm show` argv: the slice subcommand and chart ref (both
/// positional), then `--version` / `--repo`. Connection flags come from `Conn`.
fn build_argv(args: &ShowArgs) -> Vec<String> {
    let mut v = vec![args.what.as_helm().to_string(), args.chart.clone()];
    if let Some(version) = &args.version {
        v.push("--version".to_string());
        v.push(version.clone());
    }
    if let Some(repo) = &args.repo {
        v.push("--repo".to_string());
        v.push(repo.clone());
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bare() -> ShowArgs {
        ShowArgs {
            chart: "./mychart".to_string(),
            what: What::All,
            version: None,
            repo: None,
            limit: None,
        }
    }

    #[test]
    fn default_argv_is_all_plus_chart() {
        assert_eq!(build_argv(&bare()), vec!["all", "./mychart"]);
    }

    #[test]
    fn what_selects_the_slice_subcommand() {
        let mut args = bare();
        args.what = What::Values;
        assert_eq!(build_argv(&args), vec!["values", "./mychart"]);
    }

    #[test]
    fn version_and_repo_append_after_positionals() {
        let mut args = bare();
        args.chart = "bitnami/nginx".to_string();
        args.what = What::Chart;
        args.version = Some("15.1.0".to_string());
        args.repo = Some("https://charts.bitnami.com/bitnami".to_string());
        assert_eq!(
            build_argv(&args),
            vec![
                "chart",
                "bitnami/nginx",
                "--version",
                "15.1.0",
                "--repo",
                "https://charts.bitnami.com/bitnami",
            ]
        );
    }

    #[test]
    fn every_slice_maps_to_its_helm_word() {
        assert_eq!(What::All.as_helm(), "all");
        assert_eq!(What::Chart.as_helm(), "chart");
        assert_eq!(What::Values.as_helm(), "values");
        assert_eq!(What::Readme.as_helm(), "readme");
        assert_eq!(What::Crds.as_helm(), "crds");
    }
}
