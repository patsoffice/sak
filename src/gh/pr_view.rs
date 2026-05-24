use std::process::ExitCode;

use anyhow::Result;
use clap::Args;

use crate::gh::argv::ArgvBuilder;
use crate::gh::client;
use crate::gh::render::{self, Format};

/// Default `gh` field set when `--fields` is omitted. Forwarded verbatim to
/// `gh pr view --json`; sak does not invent its own field names. Validated
/// against `gh pr view --json` (with no value, gh lists the accepted fields).
const DEFAULT_FIELDS: &str = "number,title,author,body,state,mergeable,statusCheckRollup,reviews,createdAt,updatedAt,headRefName,baseRefName,additions,deletions,changedFiles,labels";

#[derive(Args)]
#[command(
    about = "Show a single pull request's metadata (read-only)",
    long_about = "Inspect a single pull request via `gh pr view <pr> --json \
        <fields>` — title, body, state, mergeability, status checks, review \
        decision, diff stats, labels, and more.\n\n\
        `<pr>` is a PR number, URL, or branch name (whatever `gh pr view` \
        accepts).\n\n\
        Output defaults to `--format json` (a PR has nested arrays — \
        `reviews`, `statusCheckRollup`, `labels`, `comments` — that don't \
        flatten cleanly into a table). `--format tsv` emits one \
        `field<TAB>value` line per requested field, rendering scalars as-is, \
        user/named objects as their `login`/`name`, atom arrays comma-joined, \
        and anything deeper as compact JSON.\n\n\
        The `--fields` value is forwarded verbatim to `gh` — sak does not \
        maintain its own field-name set, so any column `gh pr view --json` \
        accepts works here. Repository, auth, and host resolution are whatever \
        `gh` itself uses (the current directory's remote unless `--repo` is \
        given; `GH_TOKEN` / `GITHUB_TOKEN` or `~/.config/gh/hosts.yml`).",
    after_help = "\
Examples:
  sak gh pr-view 13468                               PR #13468 in the current repo, JSON
  sak gh pr-view 13468 --repo cli/cli                 A specific repo
  sak gh pr-view 13468 --format tsv                   Flat field<TAB>value lines
  sak gh pr-view 13468 --fields number,title,mergeable,reviewDecision"
)]
pub struct PrViewArgs {
    /// PR number, URL, or branch name
    #[arg(value_name = "PR")]
    pub pr: String,

    /// Repository in `owner/name` form (default: current directory's remote)
    #[arg(long, value_name = "OWNER/NAME")]
    pub repo: Option<String>,

    /// Comma-separated `gh` field names to request and project
    #[arg(long, default_value = DEFAULT_FIELDS)]
    pub fields: String,

    /// Output format
    #[arg(long, value_enum, default_value_t = Format::Json)]
    pub format: Format,

    /// Maximum number of output lines
    #[arg(long)]
    pub limit: Option<usize>,
}

pub fn run(args: &PrViewArgs) -> Result<ExitCode> {
    let fields = render::parse_fields(&args.fields);
    if fields.is_empty() {
        anyhow::bail!("--fields must name at least one gh field");
    }
    let argv = build_argv(args, &fields.join(","));

    let argv_refs: Vec<&str> = argv.iter().map(String::as_str).collect();
    let stdout = client::invoke_ok("pr", Some("view"), &argv_refs)?;

    render::emit_single_to_stdout(&stdout, &fields, args.format, args.limit)
}

/// Assemble the `gh pr view` arg vector. The PR selector is positional and
/// must precede `--json`. Split out so it can be unit-tested without spawning
/// `gh`.
fn build_argv(args: &PrViewArgs, fields_csv: &str) -> Vec<String> {
    let mut b = ArgvBuilder::new();
    b.push_value(args.pr.as_str())
        .push("--json", fields_csv)
        .push_opt("--repo", args.repo.as_deref());
    b.into_argv()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bare(pr: &str) -> PrViewArgs {
        PrViewArgs {
            pr: pr.into(),
            repo: None,
            fields: DEFAULT_FIELDS.into(),
            format: Format::Json,
            limit: None,
        }
    }

    #[test]
    fn pr_selector_precedes_json_flag() {
        let argv = build_argv(&bare("13468"), DEFAULT_FIELDS);
        assert_eq!(argv, vec!["13468", "--json", DEFAULT_FIELDS]);
    }

    #[test]
    fn repo_flag_follows_json() {
        let mut args = bare("13468");
        args.repo = Some("cli/cli".into());
        let argv = build_argv(&args, DEFAULT_FIELDS);
        assert_eq!(
            argv,
            vec!["13468", "--json", DEFAULT_FIELDS, "--repo", "cli/cli"]
        );
    }

    #[test]
    fn custom_fields_are_forwarded_verbatim() {
        let args = bare("13468");
        let fields = render::parse_fields("number, title ,mergeable");
        let argv = build_argv(&args, &fields.join(","));
        assert_eq!(argv[2], "number,title,mergeable");
    }

    #[test]
    fn default_format_is_json() {
        assert_eq!(bare("1").format, Format::Json);
    }
}
