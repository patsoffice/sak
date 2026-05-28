use crate::output::Outcome;

use anyhow::Result;
use clap::Args;

use crate::gh::argv::ArgvBuilder;
use crate::gh::client;
use crate::gh::render::{self, Format};

/// Default `gh` field set when `--fields` is omitted. Forwarded verbatim to
/// `gh workflow list --json`; sak does not invent its own field names.
const DEFAULT_FIELDS: &str = "id,name,path,state";

#[derive(Args)]
#[command(
    about = "List workflow definitions as TSV (read-only)",
    long_about = "List GitHub Actions workflow *definitions* via `gh workflow \
        list --json <fields>` and emit one TSV row per workflow. This lists \
        the workflows configured in a repo, not their runs — use `sak gh \
        run-list` for runs.\n\n\
        The `--fields` value is forwarded verbatim to `gh` — sak does not \
        maintain its own field-name set, so any column `gh workflow list \
        --json` accepts works here. Scalar fields render as-is; a user object \
        renders as its `login`; an array of named objects renders as a \
        comma-joined list of names; anything else renders as compact JSON. \
        Use `--format json` to get `gh`'s full JSON array unchanged.\n\n\
        The `state` field is `active` / `disabled_manually` / \
        `disabled_inactivity` — handy for spotting workflows GitHub \
        auto-disabled. Pass `--all` to include disabled workflows.\n\n\
        Repository, auth, and host resolution are whatever `gh` itself uses \
        (the current directory's remote unless `--repo` is given; `GH_TOKEN` \
        / `GITHUB_TOKEN` or `~/.config/gh/hosts.yml` for auth).",
    after_help = "\
Examples:
  sak gh workflow-list                               Enabled workflows in the current repo
  sak gh workflow-list --all                         Include disabled workflows
  sak gh workflow-list --repo cli/cli --limit 50
  sak gh workflow-list --fields name,state --format json"
)]
pub struct WorkflowListArgs {
    /// Repository in `owner/name` form (default: current directory's remote)
    #[arg(long, value_name = "OWNER/NAME")]
    pub repo: Option<String>,

    /// Include disabled workflows (forwarded to `gh --all`)
    #[arg(long)]
    pub all: bool,

    /// Maximum number of workflows to fetch (forwarded to `gh --limit`)
    #[arg(long)]
    pub limit: Option<usize>,

    /// Comma-separated `gh` field names to request and project
    #[arg(long, default_value = DEFAULT_FIELDS)]
    pub fields: String,

    /// Output format
    #[arg(long, value_enum, default_value_t = Format::Tsv)]
    pub format: Format,
}

pub fn run(args: &WorkflowListArgs) -> Result<Outcome> {
    let fields = render::parse_fields(&args.fields);
    if fields.is_empty() {
        anyhow::bail!("--fields must name at least one gh field");
    }
    let argv = build_argv(args, &fields.join(","));

    let argv_refs: Vec<&str> = argv.iter().map(String::as_str).collect();
    let stdout = client::invoke_ok("workflow", Some("list"), &argv_refs)?;

    render::emit_to_stdout(&stdout, &fields, args.format, args.limit)
}

/// Assemble the `gh workflow list` arg vector. Split out so it can be
/// unit-tested without spawning `gh`.
fn build_argv(args: &WorkflowListArgs, fields_csv: &str) -> Vec<String> {
    let mut b = ArgvBuilder::new();
    b.push("--json", fields_csv)
        .push_opt("--repo", args.repo.as_deref())
        .push_flag_if(args.all, "--all")
        .push_opt("--limit", args.limit.map(|n| n.to_string()).as_deref());
    b.into_argv()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bare() -> WorkflowListArgs {
        WorkflowListArgs {
            repo: None,
            all: false,
            limit: None,
            fields: DEFAULT_FIELDS.into(),
            format: Format::Tsv,
        }
    }

    #[test]
    fn default_argv_requests_default_fields_only() {
        let argv = build_argv(&bare(), DEFAULT_FIELDS);
        assert_eq!(argv, vec!["--json", DEFAULT_FIELDS]);
    }

    #[test]
    fn all_flags_map_to_gh_flags() {
        let mut args = bare();
        args.repo = Some("cli/cli".into());
        args.all = true;
        args.limit = Some(50);
        let argv = build_argv(&args, DEFAULT_FIELDS);
        assert_eq!(
            argv,
            vec![
                "--json",
                DEFAULT_FIELDS,
                "--repo",
                "cli/cli",
                "--all",
                "--limit",
                "50",
            ]
        );
    }

    #[test]
    fn custom_fields_are_forwarded_verbatim() {
        let args = bare();
        let fields = render::parse_fields("name, state ,path");
        let argv = build_argv(&args, &fields.join(","));
        assert_eq!(argv[1], "name,state,path");
    }
}
