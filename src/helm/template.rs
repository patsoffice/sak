use std::process::ExitCode;

use anyhow::Result;
use clap::Args;

use crate::helm::client::{self, Conn};
use crate::helm::emit_text_to_stdout;

#[derive(Args)]
#[command(
    about = "Render a chart's templates locally to YAML (read-only, offline)",
    long_about = "Render a chart's templates via `helm template <chart>` and emit the resulting \
        multi-document YAML.\n\n\
        `helm template` is intrinsically offline: it renders locally and never \
        contacts the cluster (sak does not pass `--validate`), so it is safe to \
        run regardless of cluster state or connectivity — this is the command \
        for \"what manifests would this chart generate?\". The chart can be a \
        repo reference, an `oci://` ref, a local directory, or a `.tgz`.\n\n\
        `--release-name` sets `.Release.Name` (helm's own default is \
        `release-name`); `--namespace` sets `.Release.Namespace`. `--values` \
        and `--set` overlay values (both repeatable); `--show-only` restricts \
        output to matching templates (repeatable); `--version` / `--repo` plumb \
        the chart reference. Pipe the output through `sak config query` for \
        selectors or `sak fs grep` for text searches.\n\n\
        Exit status: 0 when manifests are rendered, 1 when output is empty, \
        2 on error (e.g. a template error or an unresolvable chart).",
    after_help = "\
Examples:
  sak helm template ./mychart                          Render with default release-name
  sak helm template myrel ./mychart -n prod            Set release name + namespace
  sak helm template ./mychart --set replicas=5 --values prod.yaml
  sak helm template ./mychart --show-only templates/deployment.yaml
  sak helm template ./mychart | sak config query .spec.replicas --format yaml"
)]
pub struct TemplateArgs {
    /// Chart reference: `repo/chart`, an `oci://` ref, a local dir, or a `.tgz`
    #[arg(value_name = "CHART")]
    pub chart: String,

    /// Release name to render with (sets `.Release.Name`; default `release-name`)
    #[arg(long, value_name = "NAME")]
    pub release_name: Option<String>,

    /// Namespace to render with (sets `.Release.Namespace`)
    #[arg(long, short = 'n', value_name = "NS")]
    pub namespace: Option<String>,

    /// Values overlay file (repeatable, forwarded to `helm --values`)
    #[arg(long = "values", short = 'f', value_name = "FILE")]
    pub values: Vec<String>,

    /// Inline value override `key=value` (repeatable, forwarded to `helm --set`)
    #[arg(long = "set", value_name = "KEY=VALUE")]
    pub set: Vec<String>,

    /// Chart version (default: latest)
    #[arg(long, value_name = "VER")]
    pub version: Option<String>,

    /// Chart repository URL to pull from (forwarded to `helm --repo`)
    #[arg(long, value_name = "URL")]
    pub repo: Option<String>,

    /// Render only templates matching this path (repeatable, `helm --show-only`)
    #[arg(long = "show-only", short = 's', value_name = "TEMPLATE")]
    pub show_only: Vec<String>,

    /// Maximum number of output lines
    #[arg(long)]
    pub limit: Option<usize>,
}

pub fn run(args: &TemplateArgs) -> Result<ExitCode> {
    let argv = build_argv(args);
    let argv_refs: Vec<&str> = argv.iter().map(String::as_str).collect();
    // --namespace sets .Release.Namespace at render time; no cluster contact.
    let conn = Conn {
        namespace: args.namespace.as_deref(),
        ..Default::default()
    };
    let stdout = client::invoke_ok("template", None, &argv_refs, conn)?;
    emit_text_to_stdout(&stdout, args.limit)
}

/// Assemble the `helm template` argv: optional release name then the chart ref
/// (both positional, name first per `helm template [NAME] CHART`), followed by
/// the value/version/repo/show-only flags. `--namespace` rides on `Conn`.
fn build_argv(args: &TemplateArgs) -> Vec<String> {
    let mut v = Vec::new();
    if let Some(name) = &args.release_name {
        v.push(name.clone());
    }
    v.push(args.chart.clone());
    for f in &args.values {
        v.push("--values".to_string());
        v.push(f.clone());
    }
    for s in &args.set {
        v.push("--set".to_string());
        v.push(s.clone());
    }
    if let Some(version) = &args.version {
        v.push("--version".to_string());
        v.push(version.clone());
    }
    if let Some(repo) = &args.repo {
        v.push("--repo".to_string());
        v.push(repo.clone());
    }
    for t in &args.show_only {
        v.push("--show-only".to_string());
        v.push(t.clone());
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bare() -> TemplateArgs {
        TemplateArgs {
            chart: "./mychart".to_string(),
            release_name: None,
            namespace: None,
            values: vec![],
            set: vec![],
            version: None,
            repo: None,
            show_only: vec![],
            limit: None,
        }
    }

    #[test]
    fn default_argv_is_just_the_chart() {
        // No release name -> helm uses its own `release-name` default.
        assert_eq!(build_argv(&bare()), vec!["./mychart"]);
    }

    #[test]
    fn release_name_precedes_chart() {
        let mut args = bare();
        args.release_name = Some("myrel".to_string());
        assert_eq!(build_argv(&args), vec!["myrel", "./mychart"]);
    }

    #[test]
    fn values_and_set_are_repeatable_after_chart() {
        let mut args = bare();
        args.values = vec!["a.yaml".to_string(), "b.yaml".to_string()];
        args.set = vec!["replicas=5".to_string(), "image.tag=1.2".to_string()];
        assert_eq!(
            build_argv(&args),
            vec![
                "./mychart",
                "--values",
                "a.yaml",
                "--values",
                "b.yaml",
                "--set",
                "replicas=5",
                "--set",
                "image.tag=1.2",
            ]
        );
    }

    #[test]
    fn version_repo_and_show_only_forward() {
        let mut args = bare();
        args.chart = "bitnami/nginx".to_string();
        args.version = Some("15.1.0".to_string());
        args.repo = Some("https://charts.bitnami.com/bitnami".to_string());
        args.show_only = vec!["templates/deployment.yaml".to_string()];
        assert_eq!(
            build_argv(&args),
            vec![
                "bitnami/nginx",
                "--version",
                "15.1.0",
                "--repo",
                "https://charts.bitnami.com/bitnami",
                "--show-only",
                "templates/deployment.yaml",
            ]
        );
    }
}
