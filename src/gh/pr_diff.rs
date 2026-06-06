use crate::output::Outcome;
use std::io;

use anyhow::Result;
use clap::Args;

use crate::gh::argv::ArgvBuilder;
use crate::gh::client;
use crate::output::BoundedWriter;

#[derive(Args)]
#[command(
    about = "Show a pull request's unified diff, or its changed file list (read-only)",
    long_about = "Fetch a pull request's diff via `gh pr diff <pr>`. By default the \
        full unified diff is streamed through sak's bounded writer (so `--limit` \
        truncates cleanly); `--name-only` lists just the changed file paths, one \
        per line.\n\n\
        `<pr>` is a PR number, URL, or branch name (whatever `gh pr diff` \
        accepts). This complements `sak gh pr-view` (metadata) — reach for it \
        instead of the `sak gh api` escape hatch with the \
        `application/vnd.github.v3.diff` media type.\n\n\
        Repository, auth, and host resolution are whatever `gh` itself uses (the \
        current directory's remote unless `--repo` is given; `GH_TOKEN` / \
        `GITHUB_TOKEN` or `~/.config/gh/hosts.yml`).",
    after_help = "\
Examples:
  sak gh pr-diff 13468                                Full unified diff
  sak gh pr-diff 13468 --repo cli/cli                 A specific repo
  sak gh pr-diff 13468 --name-only                    Just the changed file paths
  sak gh pr-diff 13468 --limit 200                    Truncate to the first 200 lines"
)]
pub struct PrDiffArgs {
    /// PR number, URL, or branch name
    #[arg(value_name = "PR")]
    pub pr: String,

    /// Repository in `owner/name` form (default: current directory's remote)
    #[arg(long, value_name = "OWNER/NAME")]
    pub repo: Option<String>,

    /// List only the changed file paths instead of the full diff
    #[arg(long)]
    pub name_only: bool,

    /// Maximum number of output lines
    #[arg(long)]
    pub limit: Option<usize>,
}

pub fn run(args: &PrDiffArgs) -> Result<Outcome> {
    let argv = build_argv(args);
    let argv_refs: Vec<&str> = argv.iter().map(String::as_str).collect();
    let stdout = client::invoke_ok("pr", Some("diff"), &argv_refs)?;
    emit_diff(&stdout, args.limit)
}

/// Assemble the `gh pr diff` arg vector. The PR selector is positional and
/// leads. Split out so it can be unit-tested without spawning `gh`.
fn build_argv(args: &PrDiffArgs) -> Vec<String> {
    let mut b = ArgvBuilder::new();
    b.push_value(args.pr.as_str())
        .push_opt("--repo", args.repo.as_deref())
        .push_flag_if(args.name_only, "--name-only");
    b.into_argv()
}

/// Stream raw diff text through the bounded writer, one line at a time, so
/// `--limit` truncates cleanly. Empty output (an empty diff, e.g. a PR whose
/// branches match) maps to sak's exit code 1.
fn emit_diff(stdout: &[u8], limit: Option<usize>) -> Result<Outcome> {
    let text = String::from_utf8_lossy(stdout);
    if text.trim().is_empty() {
        return Ok(Outcome::NotFound);
    }
    let out = io::stdout();
    let handle = out.lock();
    let mut writer = BoundedWriter::new(handle, limit);
    for line in text.split_inclusive('\n') {
        let line = line.strip_suffix('\n').unwrap_or(line);
        if !writer.write_line(line)? {
            break;
        }
    }
    writer.flush()?;
    Ok(Outcome::Found)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bare(pr: &str) -> PrDiffArgs {
        PrDiffArgs {
            pr: pr.into(),
            repo: None,
            name_only: false,
            limit: None,
        }
    }

    #[test]
    fn bare_pr_passes_only_the_selector() {
        let argv = build_argv(&bare("13468"));
        assert_eq!(argv, vec!["13468"]);
        assert!(!argv.iter().any(|a| a == "--name-only"));
    }

    #[test]
    fn name_only_appends_flag() {
        let mut args = bare("13468");
        args.name_only = true;
        let argv = build_argv(&args);
        assert_eq!(argv, vec!["13468", "--name-only"]);
    }

    #[test]
    fn repo_precedes_name_only() {
        let mut args = bare("13468");
        args.repo = Some("cli/cli".into());
        args.name_only = true;
        let argv = build_argv(&args);
        assert_eq!(argv, vec!["13468", "--repo", "cli/cli", "--name-only"]);
    }
}
