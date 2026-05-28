use crate::output::Outcome;

use anyhow::Result;
use clap::Args;

use crate::gh::argv::ArgvBuilder;
use crate::gh::client;
use crate::gh::render::{self, Format};

/// Default `gh` field set when `--fields` is omitted. Forwarded verbatim to
/// `gh run list --json`; sak does not invent its own field names. `databaseId`
/// leads because it's the run ID `sak gh run-view` consumes.
const DEFAULT_FIELDS: &str = "databaseId,workflowName,headBranch,event,status,conclusion,createdAt,startedAt,updatedAt,displayTitle";

#[derive(Args)]
#[command(
    about = "List workflow runs as TSV (read-only)",
    long_about = "List GitHub Actions workflow runs via `gh run list --json \
        <fields>` and emit one TSV row per run — the workhorse command for CI \
        triage.\n\n\
        The `--fields` value is forwarded verbatim to `gh` — sak does not \
        maintain its own field-name set, so any column `gh run list --json` \
        accepts works here. Scalar fields render as-is; a user object renders \
        as its `login`; an array of named objects renders as a comma-joined \
        list of names; anything else renders as compact JSON. Use `--format \
        json` to get `gh`'s full JSON array unchanged.\n\n\
        The default field set leads with `databaseId` — that's the run ID \
        `sak gh run-view` needs.\n\n\
        Repository, auth, and host resolution are whatever `gh` itself uses \
        (the current directory's remote unless `--repo` is given; `GH_TOKEN` \
        / `GITHUB_TOKEN` or `~/.config/gh/hosts.yml` for auth).",
    after_help = "\
Examples:
  sak gh run-list                                    Most recent runs in the current repo
  sak gh run-list --workflow ci.yml --branch main
  sak gh run-list --status completed --event push --limit 50
  sak gh run-list --user octocat --status in_progress
  sak gh run-list --fields databaseId,status,conclusion --format json"
)]
pub struct RunListArgs {
    /// Repository in `owner/name` form (default: current directory's remote)
    #[arg(long, value_name = "OWNER/NAME")]
    pub repo: Option<String>,

    /// Filter by workflow name or ID
    #[arg(long, value_name = "NAME-OR-ID")]
    pub workflow: Option<String>,

    /// Filter by branch
    #[arg(long, value_name = "NAME")]
    pub branch: Option<String>,

    /// Filter by trigger event (e.g. push, pull_request, schedule)
    #[arg(long, value_name = "EVENT")]
    pub event: Option<String>,

    /// Filter by run status (e.g. queued, in_progress, completed)
    #[arg(long, value_name = "STATUS")]
    pub status: Option<String>,

    /// Filter by the actor (user) that triggered the run
    #[arg(long, value_name = "USER")]
    pub user: Option<String>,

    /// Maximum number of runs to fetch (forwarded to `gh --limit`)
    #[arg(long, default_value_t = 30)]
    pub limit: usize,

    /// Comma-separated `gh` field names to request and project
    #[arg(long, default_value = DEFAULT_FIELDS)]
    pub fields: String,

    /// Output format
    #[arg(long, value_enum, default_value_t = Format::Tsv)]
    pub format: Format,
}

pub fn run(args: &RunListArgs) -> Result<Outcome> {
    let fields = render::parse_fields(&args.fields);
    if fields.is_empty() {
        anyhow::bail!("--fields must name at least one gh field");
    }
    let argv = build_argv(args, &fields.join(","));

    let argv_refs: Vec<&str> = argv.iter().map(String::as_str).collect();
    let stdout = client::invoke_ok("run", Some("list"), &argv_refs)?;

    render::emit_to_stdout(&stdout, &fields, args.format, Some(args.limit))
}

/// Assemble the `gh run list` arg vector. Split out so it can be unit-tested
/// without spawning `gh`.
fn build_argv(args: &RunListArgs, fields_csv: &str) -> Vec<String> {
    let mut b = ArgvBuilder::new();
    b.push("--json", fields_csv)
        .push_opt("--repo", args.repo.as_deref())
        .push_opt("--workflow", args.workflow.as_deref())
        .push_opt("--branch", args.branch.as_deref())
        .push_opt("--event", args.event.as_deref())
        .push_opt("--status", args.status.as_deref())
        .push_opt("--user", args.user.as_deref())
        .push("--limit", &args.limit.to_string());
    b.into_argv()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bare() -> RunListArgs {
        RunListArgs {
            repo: None,
            workflow: None,
            branch: None,
            event: None,
            status: None,
            user: None,
            limit: 30,
            fields: DEFAULT_FIELDS.into(),
            format: Format::Tsv,
        }
    }

    #[test]
    fn default_argv_requests_default_fields_and_limit_30() {
        let argv = build_argv(&bare(), DEFAULT_FIELDS);
        assert_eq!(argv[0], "--json");
        assert_eq!(argv[1], DEFAULT_FIELDS);
        // No filters, so --limit is the only trailing pair.
        assert_eq!(argv[2], "--limit");
        assert_eq!(argv[3], "30");
        // databaseId leads — it's the run-view input.
        assert!(argv[1].starts_with("databaseId"));
    }

    #[test]
    fn all_filters_map_to_gh_flags() {
        let mut args = bare();
        args.repo = Some("cli/cli".into());
        args.workflow = Some("ci.yml".into());
        args.branch = Some("main".into());
        args.event = Some("push".into());
        args.status = Some("completed".into());
        args.user = Some("octocat".into());
        args.limit = 50;
        let argv = build_argv(&args, DEFAULT_FIELDS);
        assert_eq!(
            argv,
            vec![
                "--json",
                DEFAULT_FIELDS,
                "--repo",
                "cli/cli",
                "--workflow",
                "ci.yml",
                "--branch",
                "main",
                "--event",
                "push",
                "--status",
                "completed",
                "--user",
                "octocat",
                "--limit",
                "50",
            ]
        );
    }

    #[test]
    fn custom_fields_are_forwarded_verbatim() {
        let args = bare();
        let fields = render::parse_fields("databaseId, status ,conclusion");
        let argv = build_argv(&args, &fields.join(","));
        assert_eq!(argv[1], "databaseId,status,conclusion");
    }
}
