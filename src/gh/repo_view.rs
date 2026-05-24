use std::process::ExitCode;

use anyhow::Result;
use clap::Args;

use crate::gh::argv::ArgvBuilder;
use crate::gh::client;
use crate::gh::render::{self, Format};

/// Default `gh` field set when `--fields` is omitted. Forwarded verbatim to
/// `gh repo view --json`; sak does not invent its own field names.
const DEFAULT_FIELDS: &str = "name,nameWithOwner,owner,description,isPrivate,isArchived,defaultBranchRef,primaryLanguage,languages,repositoryTopics,licenseInfo,stargazerCount,forkCount,pushedAt,updatedAt,url";

#[derive(Args)]
#[command(
    about = "Show repository metadata (read-only)",
    long_about = "Inspect a repository's metadata via `gh repo view [<owner/name>] \
        --json <fields>` — default branch, description, primary language, \
        topics, visibility, license, star/fork counts, and more.\n\n\
        With no positional argument this targets the current directory's git \
        remote, exactly like `gh repo view`.\n\n\
        Output defaults to `--format json` (the metadata has nested objects \
        and arrays — `defaultBranchRef`, `languages`, `repositoryTopics`, \
        `licenseInfo` — that don't flatten cleanly into a table). `--format \
        tsv` emits one \
        `field<TAB>value` line per requested field, rendering scalars as-is, \
        user/named objects as their `login`/`name`, atom arrays comma-joined, \
        and anything deeper as compact JSON.\n\n\
        The `--fields` value is forwarded verbatim to `gh` — sak does not \
        maintain its own field-name set, so any column `gh repo view --json` \
        accepts works here. Auth and host resolution are whatever `gh` itself \
        uses (`GH_TOKEN` / `GITHUB_TOKEN` or `~/.config/gh/hosts.yml`).",
    after_help = "\
Examples:
  sak gh repo-view                                   Current repo, JSON
  sak gh repo-view cli/cli                            A specific repo, JSON
  sak gh repo-view cli/cli --format tsv               Flat field<TAB>value lines
  sak gh repo-view --fields nameWithOwner,defaultBranchRef,stargazerCount"
)]
pub struct RepoViewArgs {
    /// Repository in `owner/name` form (default: current directory's remote)
    #[arg(value_name = "OWNER/NAME")]
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

pub fn run(args: &RepoViewArgs) -> Result<ExitCode> {
    let fields = render::parse_fields(&args.fields);
    if fields.is_empty() {
        anyhow::bail!("--fields must name at least one gh field");
    }
    let argv = build_argv(args, &fields.join(","));

    let argv_refs: Vec<&str> = argv.iter().map(String::as_str).collect();
    let stdout = client::invoke_ok("repo", Some("view"), &argv_refs)?;

    render::emit_single_to_stdout(&stdout, &fields, args.format, args.limit)
}

/// Assemble the `gh repo view` arg vector. The optional repo is positional and
/// must precede `--json`. Split out so it can be unit-tested without spawning
/// `gh`.
fn build_argv(args: &RepoViewArgs, fields_csv: &str) -> Vec<String> {
    let mut b = ArgvBuilder::new();
    if let Some(repo) = &args.repo {
        b.push_value(repo.as_str());
    }
    b.push("--json", fields_csv);
    b.into_argv()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bare() -> RepoViewArgs {
        RepoViewArgs {
            repo: None,
            fields: DEFAULT_FIELDS.into(),
            format: Format::Json,
            limit: None,
        }
    }

    #[test]
    fn no_repo_omits_positional() {
        let argv = build_argv(&bare(), DEFAULT_FIELDS);
        assert_eq!(argv, vec!["--json", DEFAULT_FIELDS]);
    }

    #[test]
    fn repo_positional_precedes_json_flag() {
        let mut args = bare();
        args.repo = Some("cli/cli".into());
        let argv = build_argv(&args, DEFAULT_FIELDS);
        assert_eq!(argv, vec!["cli/cli", "--json", DEFAULT_FIELDS]);
    }

    #[test]
    fn custom_fields_are_forwarded_verbatim() {
        let args = bare();
        let fields = render::parse_fields("nameWithOwner, stargazerCount ,url");
        let argv = build_argv(&args, &fields.join(","));
        assert_eq!(argv[1], "nameWithOwner,stargazerCount,url");
    }

    #[test]
    fn default_format_is_json() {
        assert_eq!(bare().format, Format::Json);
    }
}
