use std::process::ExitCode;

use anyhow::Result;
use clap::Args;

use crate::nix::client;
use crate::output::BoundedWriter;

#[derive(Args)]
#[command(
    about = "List a store path's references / referrers / closure (read-only)",
    long_about = "List the store paths related to a given store path via `nix-store --query`, \
        one path per line. Three mutually exclusive modes:\n  \
        • default — direct references *out* (what this path depends on);\n  \
        • `--referrers` — direct referrers *in* (what depends on this path);\n  \
        • `--closure` — the transitive closure / requisites (every path reachable \
        by following references, including the path itself).\n\n\
        `<path>` is a store path (e.g. `/nix/store/…-foo`) or a symlink into the \
        store. Output is sorted for determinism.\n\n\
        This is the one nix command sak shells out to `nix-store` (a separate \
        binary) for: reverse dependencies (`--referrers`) have no modern `nix` \
        subcommand. The chokepoint only ever runs `nix-store --query` with these \
        read-only sub-flags.\n\n\
        Exit status: 0 when at least one path is listed, 1 when none (e.g. a path \
        with no references, or nothing depends on it), 2 on error.",
    after_help = "\
Examples:
  sak nix references /nix/store/…-hello        Direct dependencies of a path
  sak nix references --referrers /nix/store/…-glibc   What depends on glibc
  sak nix references --closure /nix/store/…-hello     Full transitive closure"
)]
pub struct ReferencesArgs {
    /// Store path (or a symlink into the store) to query
    #[arg(value_name = "PATH")]
    pub path: String,

    /// List direct referrers (what depends on this) instead of references
    #[arg(long, conflicts_with = "closure")]
    pub referrers: bool,

    /// List the transitive closure (requisites) instead of direct references
    #[arg(long, conflicts_with = "referrers")]
    pub closure: bool,

    /// Maximum number of output lines (bounds output, not the nix-store query)
    #[arg(long)]
    pub limit: Option<usize>,
}

pub fn run(args: &ReferencesArgs) -> Result<ExitCode> {
    let flag = if args.referrers {
        "--referrers"
    } else if args.closure {
        "--requisites"
    } else {
        "--references"
    };
    let stdout = client::nix_store_query(flag, &args.path)?;

    let text = String::from_utf8_lossy(&stdout);
    let mut paths: Vec<&str> = text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    paths.sort_unstable();
    paths.dedup();

    if paths.is_empty() {
        return Ok(ExitCode::from(1));
    }

    let out = std::io::stdout();
    let mut writer = BoundedWriter::new(out.lock(), args.limit);
    for path in &paths {
        if !writer.write_line(path)? {
            break;
        }
    }
    writer.flush()?;
    Ok(ExitCode::SUCCESS)
}

#[cfg(test)]
mod tests {
    /// The mode flags map to the right `nix-store --query` sub-flag. Mirrors the
    /// selection in `run` (kept in lockstep with it).
    fn flag_for(referrers: bool, closure: bool) -> &'static str {
        if referrers {
            "--referrers"
        } else if closure {
            "--requisites"
        } else {
            "--references"
        }
    }

    #[test]
    fn default_mode_is_references() {
        assert_eq!(flag_for(false, false), "--references");
    }

    #[test]
    fn referrers_and_closure_select_their_flags() {
        assert_eq!(flag_for(true, false), "--referrers");
        assert_eq!(flag_for(false, true), "--requisites");
    }
}
