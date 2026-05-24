use std::process::ExitCode;

use anyhow::Result;
use clap::{Args, ValueEnum};

use crate::helm::client::{self, Conn};
use crate::helm::emit_text_to_stdout;

/// Which slice of a release `helm get` should dump. These are `helm get`'s
/// positional subcommands (`helm get manifest <release>`), forwarded as the
/// first argument; the chokepoint's `get` family is read-only, so each is safe.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum What {
    /// Everything: a human-oriented dump of metadata, values, and manifest
    All,
    /// The rendered Kubernetes manifest (YAML)
    Manifest,
    /// The release's values (YAML)
    Values,
    /// The rendered NOTES.txt (plain text)
    Notes,
    /// The release's hooks (YAML)
    Hooks,
}

impl What {
    /// The `helm get` subcommand word.
    fn as_helm(self) -> &'static str {
        match self {
            What::All => "all",
            What::Manifest => "manifest",
            What::Values => "values",
            What::Notes => "notes",
            What::Hooks => "hooks",
        }
    }
}

#[derive(Args)]
#[command(
    about = "Dump a slice of a Helm release (manifest / values / notes / hooks)",
    long_about = "Dump a slice of a deployed Helm release via `helm get <what> <release>`.\n\n\
        `--what` selects the slice (default `all`): `manifest` is the rendered \
        Kubernetes YAML, `values` is the release's values (emitted as header-free \
        YAML so it pipes cleanly into `sak config`), `notes` is the rendered \
        NOTES.txt, `hooks` is the hook manifests, and `all` is helm's combined \
        human-oriented dump. Output is helm's native text forwarded verbatim. \
        `--revision` dumps a specific past revision.\n\n\
        Cluster, auth, and namespace resolution are whatever `helm` itself \
        uses (`KUBECONFIG` / `~/.kube/config`).\n\n\
        Exit status: 0 when output is produced, 1 when the release does not \
        exist or the slice is empty, 2 on any other error.",
    after_help = "\
Examples:
  sak helm get cilium -n kube-system                   Combined dump (helm get all)
  sak helm get cilium -n kube-system --what manifest   Rendered manifest YAML
  sak helm get cilium -n kube-system --what values     Release values (YAML)
  sak helm get cilium -n kube-system --what values | sak config query .k8sServiceHost --format yaml
  sak helm get cilium -n kube-system --what manifest --revision 16"
)]
pub struct GetArgs {
    /// Release name to dump
    #[arg(value_name = "RELEASE")]
    pub release: String,

    /// Which slice of the release to fetch
    #[arg(long, value_enum, default_value_t = What::All)]
    pub what: What,

    /// Namespace of the release (default: the kubeconfig's current namespace)
    #[arg(long, short = 'n', value_name = "NS")]
    pub namespace: Option<String>,

    /// Dump a specific past revision instead of the latest
    #[arg(long, value_name = "N")]
    pub revision: Option<u32>,

    /// Maximum number of output lines
    #[arg(long)]
    pub limit: Option<usize>,
}

pub fn run(args: &GetArgs) -> Result<ExitCode> {
    let argv = build_argv(args);
    let argv_refs: Vec<&str> = argv.iter().map(String::as_str).collect();
    let conn = Conn {
        namespace: args.namespace.as_deref(),
        ..Default::default()
    };
    // A missing release is "no results" (exit 1); the chokepoint maps helm's
    // not-found stderr to Ok(None).
    let Some(stdout) = client::invoke_found("get", None, &argv_refs, conn)? else {
        return Ok(ExitCode::from(1));
    };
    emit_text_to_stdout(&stdout, args.limit)
}

/// Assemble the `helm get` argv: the slice subcommand, then the release name
/// (both positional), then any flags. Connection flags come from `Conn`.
fn build_argv(args: &GetArgs) -> Vec<String> {
    let mut v = vec![args.what.as_helm().to_string(), args.release.clone()];
    // `helm get values` defaults to a `table` format that prepends a
    // `USER-SUPPLIED VALUES:` header line, which is not valid YAML. Force
    // `-o yaml` so the slice is header-free and pipeable into `sak config`.
    // The other slices have no `-o` flag (manifest/notes/hooks/all are
    // already raw text), so this is values-only.
    if matches!(args.what, What::Values) {
        v.push("--output".to_string());
        v.push("yaml".to_string());
    }
    if let Some(rev) = args.revision {
        v.push("--revision".to_string());
        v.push(rev.to_string());
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bare() -> GetArgs {
        GetArgs {
            release: "cilium".to_string(),
            what: What::All,
            namespace: None,
            revision: None,
            limit: None,
        }
    }

    #[test]
    fn default_argv_is_all_plus_release() {
        assert_eq!(build_argv(&bare()), vec!["all", "cilium"]);
    }

    #[test]
    fn what_selects_the_slice_subcommand() {
        let mut args = bare();
        args.what = What::Manifest;
        assert_eq!(build_argv(&args), vec!["manifest", "cilium"]);
    }

    #[test]
    fn values_forces_yaml_output() {
        let mut args = bare();
        args.what = What::Values;
        assert_eq!(
            build_argv(&args),
            vec!["values", "cilium", "--output", "yaml"]
        );
    }

    #[test]
    fn manifest_does_not_force_output_format() {
        let mut args = bare();
        args.what = What::Manifest;
        assert_eq!(build_argv(&args), vec!["manifest", "cilium"]);
    }

    #[test]
    fn revision_appends_flag_after_positionals() {
        let mut args = bare();
        args.what = What::Values;
        args.revision = Some(16);
        assert_eq!(
            build_argv(&args),
            vec!["values", "cilium", "--output", "yaml", "--revision", "16"]
        );
    }

    #[test]
    fn every_slice_maps_to_its_helm_word() {
        assert_eq!(What::All.as_helm(), "all");
        assert_eq!(What::Manifest.as_helm(), "manifest");
        assert_eq!(What::Values.as_helm(), "values");
        assert_eq!(What::Notes.as_helm(), "notes");
        assert_eq!(What::Hooks.as_helm(), "hooks");
    }
}
