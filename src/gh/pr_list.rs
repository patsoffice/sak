use crate::output::Outcome;

use anyhow::Result;
use clap::{Args, ValueEnum};

use crate::gh::argv::ArgvBuilder;
use crate::gh::client;
use crate::gh::render::{self, Format};

/// Default `gh` field set when `--fields` is omitted. Forwarded verbatim to
/// `gh pr list --json`; sak does not invent its own field names.
const DEFAULT_FIELDS: &str =
    "number,title,author,state,createdAt,updatedAt,headRefName,baseRefName";

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum PrState {
    Open,
    Closed,
    Merged,
    All,
}

impl PrState {
    fn as_gh(self) -> &'static str {
        match self {
            PrState::Open => "open",
            PrState::Closed => "closed",
            PrState::Merged => "merged",
            PrState::All => "all",
        }
    }
}

#[derive(Args)]
#[command(
    about = "List pull requests as TSV (read-only)",
    long_about = "List pull requests via `gh pr list --json <fields>` and emit \
        one TSV row per PR with the requested columns.\n\n\
        The `--fields` value is forwarded verbatim to `gh` — sak does not \
        maintain its own field-name set, so any column `gh pr list --json` \
        accepts works here. Scalar fields render as-is; a user object (e.g. \
        `author`) renders as its `login`; an array of named objects (e.g. \
        `labels`) renders as a comma-joined list of names; anything else \
        renders as compact JSON. Use `--format json` to get `gh`'s full JSON \
        array unchanged.\n\n\
        Repository, auth, and host resolution are whatever `gh` itself uses \
        (the current directory's remote unless `--repo` is given; `GH_TOKEN` \
        / `GITHUB_TOKEN` or `~/.config/gh/hosts.yml` for auth).",
    after_help = "\
Examples:
  sak gh pr-list                                     Open PRs in the current repo
  sak gh pr-list --repo cli/cli --state all --limit 50
  sak gh pr-list --author octocat --label bug --label p1
  sak gh pr-list --fields number,title,mergeable,reviewDecision
  sak gh pr-list --state merged --format json        Raw gh JSON array"
)]
pub struct PrListArgs {
    /// Repository in `owner/name` form (default: current directory's remote)
    #[arg(long, value_name = "OWNER/NAME")]
    pub repo: Option<String>,

    /// PR state filter
    #[arg(long, value_enum, default_value_t = PrState::Open)]
    pub state: PrState,

    /// Filter by author login
    #[arg(long, value_name = "USER")]
    pub author: Option<String>,

    /// Filter by label (repeatable)
    #[arg(long = "label", value_name = "LABEL")]
    pub labels: Vec<String>,

    /// Maximum number of PRs to fetch (forwarded to `gh --limit`)
    #[arg(long)]
    pub limit: Option<usize>,

    /// Comma-separated `gh` field names to request and project
    #[arg(long, default_value = DEFAULT_FIELDS)]
    pub fields: String,

    /// Output format
    #[arg(long, value_enum, default_value_t = Format::Tsv)]
    pub format: Format,
}

pub fn run(args: &PrListArgs) -> Result<Outcome> {
    let fields = render::parse_fields(&args.fields);
    if fields.is_empty() {
        anyhow::bail!("--fields must name at least one gh field");
    }

    let argv = build_argv(args);
    let argv_refs: Vec<&str> = argv.iter().map(String::as_str).collect();
    let stdout = client::invoke_ok("pr", Some("list"), &argv_refs)?;

    render::emit_to_stdout(&stdout, &fields, args.format, args.limit)
}

/// Assemble the `gh pr list` argv from the parsed args.
fn build_argv(args: &PrListArgs) -> Vec<String> {
    let fields_csv = render::parse_fields(&args.fields).join(",");
    let mut b = ArgvBuilder::new();
    b.push("--json", &fields_csv)
        .push("--state", args.state.as_gh())
        .push_opt("--repo", args.repo.as_deref())
        .push_opt("--author", args.author.as_deref())
        .push_each("--label", &args.labels)
        .push_opt("--limit", args.limit.map(|n| n.to_string()).as_deref());
    b.into_argv()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bare() -> PrListArgs {
        PrListArgs {
            repo: None,
            state: PrState::Open,
            author: None,
            labels: vec![],
            limit: None,
            fields: DEFAULT_FIELDS.into(),
            format: Format::Tsv,
        }
    }

    #[test]
    fn default_argv_requests_default_fields_and_open_state() {
        let argv = build_argv(&bare());
        assert_eq!(argv[0], "--json");
        assert_eq!(argv[1], DEFAULT_FIELDS);
        assert_eq!(argv[2], "--state");
        assert_eq!(argv[3], "open");
    }

    #[test]
    fn all_filters_map_to_gh_flags() {
        let mut args = bare();
        args.repo = Some("cli/cli".into());
        args.state = PrState::All;
        args.author = Some("octocat".into());
        args.labels = vec!["bug".into(), "p1".into()];
        args.limit = Some(50);
        let argv = build_argv(&args);
        assert_eq!(
            argv,
            vec![
                "--json",
                DEFAULT_FIELDS,
                "--state",
                "all",
                "--repo",
                "cli/cli",
                "--author",
                "octocat",
                "--label",
                "bug",
                "--label",
                "p1",
                "--limit",
                "50",
            ]
        );
    }

    #[test]
    fn custom_fields_are_forwarded_verbatim() {
        let mut args = bare();
        args.fields = "number, title ,mergeable".into();
        let argv = build_argv(&args);
        assert_eq!(argv[1], "number,title,mergeable");
    }
}
