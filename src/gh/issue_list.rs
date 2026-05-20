use std::process::ExitCode;

use anyhow::Result;
use clap::{Args, ValueEnum};

use crate::gh::client;
use crate::gh::render::{self, Format};

/// Default `gh` field set when `--fields` is omitted. Forwarded verbatim to
/// `gh issue list --json`; sak does not invent its own field names.
const DEFAULT_FIELDS: &str = "number,title,author,state,labels,createdAt,updatedAt";

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum IssueState {
    Open,
    Closed,
    All,
}

impl IssueState {
    fn as_gh(self) -> &'static str {
        match self {
            IssueState::Open => "open",
            IssueState::Closed => "closed",
            IssueState::All => "all",
        }
    }
}

#[derive(Args)]
#[command(
    about = "List issues as TSV (read-only)",
    long_about = "List issues via `gh issue list --json <fields>` and emit one \
        TSV row per issue with the requested columns.\n\n\
        The `--fields` value is forwarded verbatim to `gh` — sak does not \
        maintain its own field-name set, so any column `gh issue list --json` \
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
  sak gh issue-list                                  Open issues in the current repo
  sak gh issue-list --repo cli/cli --state all --limit 50
  sak gh issue-list --assignee octocat --label bug --label p1
  sak gh issue-list --milestone v1 --mention octocat
  sak gh issue-list --fields number,title,state,closedAt --format json"
)]
pub struct IssueListArgs {
    /// Repository in `owner/name` form (default: current directory's remote)
    #[arg(long, value_name = "OWNER/NAME")]
    pub repo: Option<String>,

    /// Issue state filter
    #[arg(long, value_enum, default_value_t = IssueState::Open)]
    pub state: IssueState,

    /// Filter by author login
    #[arg(long, value_name = "USER")]
    pub author: Option<String>,

    /// Filter by assignee login
    #[arg(long, value_name = "USER")]
    pub assignee: Option<String>,

    /// Filter to issues mentioning this login
    #[arg(long, value_name = "USER")]
    pub mention: Option<String>,

    /// Filter by label (repeatable)
    #[arg(long = "label", value_name = "LABEL")]
    pub labels: Vec<String>,

    /// Filter by milestone (number or title)
    #[arg(long, value_name = "NAME")]
    pub milestone: Option<String>,

    /// Maximum number of issues to fetch (forwarded to `gh --limit`)
    #[arg(long)]
    pub limit: Option<usize>,

    /// Comma-separated `gh` field names to request and project
    #[arg(long, default_value = DEFAULT_FIELDS)]
    pub fields: String,

    /// Output format
    #[arg(long, value_enum, default_value_t = Format::Tsv)]
    pub format: Format,
}

pub fn run(args: &IssueListArgs) -> Result<ExitCode> {
    let fields = render::parse_fields(&args.fields);
    if fields.is_empty() {
        anyhow::bail!("--fields must name at least one gh field");
    }
    let argv = build_argv(args, &fields.join(","));

    let argv_refs: Vec<&str> = argv.iter().map(String::as_str).collect();
    let stdout = client::invoke_ok("issue", Some("list"), &argv_refs)?;

    render::emit_to_stdout(&stdout, &fields, args.format, args.limit)
}

/// Assemble the `gh issue list` arg vector. Split out so it can be unit-tested
/// without spawning `gh`.
fn build_argv(args: &IssueListArgs, fields_csv: &str) -> Vec<String> {
    let mut argv: Vec<String> = vec![
        "--json".into(),
        fields_csv.to_string(),
        "--state".into(),
        args.state.as_gh().into(),
    ];
    if let Some(repo) = &args.repo {
        argv.push("--repo".into());
        argv.push(repo.clone());
    }
    if let Some(author) = &args.author {
        argv.push("--author".into());
        argv.push(author.clone());
    }
    if let Some(assignee) = &args.assignee {
        argv.push("--assignee".into());
        argv.push(assignee.clone());
    }
    if let Some(mention) = &args.mention {
        argv.push("--mention".into());
        argv.push(mention.clone());
    }
    for label in &args.labels {
        argv.push("--label".into());
        argv.push(label.clone());
    }
    if let Some(milestone) = &args.milestone {
        argv.push("--milestone".into());
        argv.push(milestone.clone());
    }
    if let Some(limit) = args.limit {
        argv.push("--limit".into());
        argv.push(limit.to_string());
    }
    argv
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bare() -> IssueListArgs {
        IssueListArgs {
            repo: None,
            state: IssueState::Open,
            author: None,
            assignee: None,
            mention: None,
            labels: vec![],
            milestone: None,
            limit: None,
            fields: DEFAULT_FIELDS.into(),
            format: Format::Tsv,
        }
    }

    #[test]
    fn default_argv_requests_default_fields_and_open_state() {
        let argv = build_argv(&bare(), DEFAULT_FIELDS);
        assert_eq!(argv[0], "--json");
        assert_eq!(argv[1], DEFAULT_FIELDS);
        assert_eq!(argv[2], "--state");
        assert_eq!(argv[3], "open");
    }

    #[test]
    fn all_filters_map_to_gh_flags() {
        let mut args = bare();
        args.repo = Some("cli/cli".into());
        args.state = IssueState::All;
        args.author = Some("octocat".into());
        args.assignee = Some("hubot".into());
        args.mention = Some("monalisa".into());
        args.labels = vec!["bug".into(), "p1".into()];
        args.milestone = Some("v1".into());
        args.limit = Some(50);
        let argv = build_argv(&args, DEFAULT_FIELDS);
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
                "--assignee",
                "hubot",
                "--mention",
                "monalisa",
                "--label",
                "bug",
                "--label",
                "p1",
                "--milestone",
                "v1",
                "--limit",
                "50",
            ]
        );
    }

    #[test]
    fn custom_fields_are_forwarded_verbatim() {
        let args = bare();
        let fields = render::parse_fields("number, title ,closedAt");
        let argv = build_argv(&args, &fields.join(","));
        assert_eq!(argv[1], "number,title,closedAt");
    }
}
