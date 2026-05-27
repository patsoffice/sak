use std::process::ExitCode;

use anyhow::Result;
use clap::Args;

use crate::nix::client;
use crate::nix::{Format, emit_to_stdout};

#[derive(Args)]
#[command(
    about = "Show a derivation's JSON (read-only)",
    long_about = "Show the derivation(s) for an installable / store path via `nix derivation \
        show`, passing nix's JSON through verbatim. The structure is too irregular \
        to flatten into a useful TSV, so this is JSON-only — pipe it through `sak \
        json query` for projections.\n\n\
        `<installable>` defaults to `.` (the current flake's default package) and \
        accepts anything `nix` does: a flake attr (`.#hello`, `nixpkgs#hello`), a \
        `.drv` path, or a realised store path (nix resolves its deriver). \
        `--recursive` includes the dependency derivations (the full `.drv` \
        closure), not just the top-level one.\n\n\
        Modern nix wraps the output as `{\"derivations\": {<drv-path>: {...}}, \
        \"version\": N}`.\n\n\
        Exit status: 0 on success, 2 on error (e.g. an installable with no \
        derivation, or an unresolvable reference).",
    after_help = "\
Examples:
  sak nix derivation-show                       Derivation of the flake's default package
  sak nix derivation-show .#hello               A specific flake attr
  sak nix derivation-show /nix/store/…-foo      Resolve a store path's deriver
  sak nix derivation-show .#hello --recursive   Include dependency derivations
  sak nix derivation-show .#hello | sak json query .derivations"
)]
pub struct DerivationShowArgs {
    /// Installable / store path / `.drv` to show (default: `.`)
    #[arg(value_name = "INSTALLABLE", default_value = ".")]
    pub installable: String,

    /// Include the dependency derivations (the full `.drv` closure)
    #[arg(long)]
    pub recursive: bool,

    /// Maximum number of output lines (bounds output, not the nix call)
    #[arg(long)]
    pub limit: Option<usize>,
}

pub fn run(args: &DerivationShowArgs) -> Result<ExitCode> {
    let argv = build_argv(args);
    let argv_refs: Vec<&str> = argv.iter().map(String::as_str).collect();
    let stdout = client::invoke_ok("derivation", Some("show"), &argv_refs)?;
    // JSON-only passthrough; the tsv closure is never reached for Format::Json.
    emit_to_stdout(&stdout, Format::Json, args.limit, "{}", |_, _| Ok(false))
}

/// Assemble the `nix derivation show` argv (the chokepoint supplies the verb,
/// subverb, and experimental-features flags). `--recursive` precedes the
/// installable, which goes last. Pure so it's unit-testable.
fn build_argv(args: &DerivationShowArgs) -> Vec<String> {
    let mut v = Vec::new();
    if args.recursive {
        v.push("--recursive".to_string());
    }
    v.push(args.installable.clone());
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bare() -> DerivationShowArgs {
        DerivationShowArgs {
            installable: ".".to_string(),
            recursive: false,
            limit: None,
        }
    }

    #[test]
    fn default_argv_is_just_the_installable() {
        assert_eq!(build_argv(&bare()), vec!["."]);
    }

    #[test]
    fn recursive_precedes_the_installable() {
        let mut a = bare();
        a.recursive = true;
        a.installable = ".#hello".to_string();
        assert_eq!(build_argv(&a), vec!["--recursive", ".#hello"]);
    }
}
