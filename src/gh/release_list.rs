use crate::output::Outcome;

use anyhow::Result;
use clap::Args;

use crate::gh::argv::ArgvBuilder;
use crate::gh::client;
use crate::gh::render::{self, Format};

/// Default `gh` field set when `--fields` is omitted. Forwarded verbatim to
/// `gh release list --json`; sak does not invent its own field names.
const DEFAULT_FIELDS: &str = "tagName,name,isDraft,isPrerelease,isLatest,publishedAt,createdAt";

#[derive(Args)]
#[command(
    about = "List releases as TSV (read-only)",
    long_about = "List releases via `gh release list --json <fields>` and emit \
        one TSV row per release with the requested columns.\n\n\
        The `--fields` value is forwarded verbatim to `gh` — sak does not \
        maintain its own field-name set, so any column `gh release list \
        --json` accepts works here. Scalar fields render as-is; a user object \
        (e.g. `author`) renders as its `login`; an array of named objects \
        renders as a comma-joined list of names; anything else renders as \
        compact JSON. Use `--format json` to get `gh`'s full JSON array \
        unchanged.\n\n\
        Repository, auth, and host resolution are whatever `gh` itself uses \
        (the current directory's remote unless `--repo` is given; `GH_TOKEN` \
        / `GITHUB_TOKEN` or `~/.config/gh/hosts.yml` for auth).",
    after_help = "\
Examples:
  sak gh release-list                                Releases in the current repo
  sak gh release-list --repo cli/cli --limit 50
  sak gh release-list --exclude-drafts --exclude-pre-releases
  sak gh release-list --fields tagName,publishedAt --format json"
)]
pub struct ReleaseListArgs {
    /// Repository in `owner/name` form (default: current directory's remote)
    #[arg(long, value_name = "OWNER/NAME")]
    pub repo: Option<String>,

    /// Maximum number of releases to fetch (forwarded to `gh --limit`)
    #[arg(long)]
    pub limit: Option<usize>,

    /// Exclude draft releases (forwarded to `gh --exclude-drafts`)
    #[arg(long)]
    pub exclude_drafts: bool,

    /// Exclude pre-releases (forwarded to `gh --exclude-pre-releases`)
    #[arg(long)]
    pub exclude_pre_releases: bool,

    /// Comma-separated `gh` field names to request and project
    #[arg(long, default_value = DEFAULT_FIELDS)]
    pub fields: String,

    /// Output format
    #[arg(long, value_enum, default_value_t = Format::Tsv)]
    pub format: Format,
}

pub fn run(args: &ReleaseListArgs) -> Result<Outcome> {
    let fields = render::parse_fields(&args.fields);
    if fields.is_empty() {
        anyhow::bail!("--fields must name at least one gh field");
    }
    let argv = build_argv(args, &fields.join(","));

    let argv_refs: Vec<&str> = argv.iter().map(String::as_str).collect();
    let stdout = client::invoke_ok("release", Some("list"), &argv_refs)?;

    render::emit_to_stdout(&stdout, &fields, args.format, args.limit)
}

/// Assemble the `gh release list` arg vector. Split out so it can be
/// unit-tested without spawning `gh`.
fn build_argv(args: &ReleaseListArgs, fields_csv: &str) -> Vec<String> {
    let mut b = ArgvBuilder::new();
    b.push("--json", fields_csv)
        .push_opt("--repo", args.repo.as_deref())
        .push_flag_if(args.exclude_drafts, "--exclude-drafts")
        .push_flag_if(args.exclude_pre_releases, "--exclude-pre-releases")
        .push_opt("--limit", args.limit.map(|n| n.to_string()).as_deref());
    b.into_argv()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bare() -> ReleaseListArgs {
        ReleaseListArgs {
            repo: None,
            limit: None,
            exclude_drafts: false,
            exclude_pre_releases: false,
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
        args.exclude_drafts = true;
        args.exclude_pre_releases = true;
        args.limit = Some(50);
        let argv = build_argv(&args, DEFAULT_FIELDS);
        assert_eq!(
            argv,
            vec![
                "--json",
                DEFAULT_FIELDS,
                "--repo",
                "cli/cli",
                "--exclude-drafts",
                "--exclude-pre-releases",
                "--limit",
                "50",
            ]
        );
    }

    #[test]
    fn custom_fields_are_forwarded_verbatim() {
        let args = bare();
        let fields = render::parse_fields("tagName, publishedAt ,isDraft");
        let argv = build_argv(&args, &fields.join(","));
        assert_eq!(argv[1], "tagName,publishedAt,isDraft");
    }
}
