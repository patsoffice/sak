use std::process::ExitCode;

use anyhow::Result;
use clap::Args;

use crate::gh::client;
use crate::gh::render::{self, Format};

/// Default `gh` field set when `--fields` is omitted. Forwarded verbatim to
/// `gh release view --json`; sak does not invent its own field names.
/// Validated against `gh release view --json` (with no value, gh lists the
/// accepted fields). Note `author` is valid here even though it is *not* a
/// `gh release list --json` field — view and list expose different sets.
const DEFAULT_FIELDS: &str =
    "tagName,name,body,isDraft,isPrerelease,publishedAt,createdAt,author,assets,url";

#[derive(Args)]
#[command(
    about = "Show a single release's metadata (read-only)",
    long_about = "Inspect a single release via `gh release view [<tag>] --json \
        <fields>` — name, body, draft/prerelease flags, publish dates, author, \
        and assets.\n\n\
        With no positional argument this targets the repository's latest \
        release, exactly like `gh release view`.\n\n\
        Output defaults to `--format json` (the `assets` array — each entry \
        has name/size/downloadCount/contentType — doesn't flatten cleanly \
        into a table). `--format tsv` emits one `field<TAB>value` line per \
        requested field, rendering scalars as-is, user/named objects as their \
        `login`/`name`, atom arrays comma-joined, and anything deeper as \
        compact JSON.\n\n\
        The `--fields` value is forwarded verbatim to `gh` — sak does not \
        maintain its own field-name set, so any column `gh release view \
        --json` accepts works here. Repository, auth, and host resolution are \
        whatever `gh` itself uses (the current directory's remote unless \
        `--repo` is given; `GH_TOKEN` / `GITHUB_TOKEN` or \
        `~/.config/gh/hosts.yml`).",
    after_help = "\
Examples:
  sak gh release-view                                Latest release in the current repo, JSON
  sak gh release-view v2.92.0 --repo cli/cli          A specific tag
  sak gh release-view v2.92.0 --format tsv            Flat field<TAB>value lines
  sak gh release-view --fields tagName,name,assets    Inspect release assets (JSON)"
)]
pub struct ReleaseViewArgs {
    /// Release tag (default: the repository's latest release)
    #[arg(value_name = "TAG")]
    pub tag: Option<String>,

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

pub fn run(args: &ReleaseViewArgs) -> Result<ExitCode> {
    let fields = render::parse_fields(&args.fields);
    if fields.is_empty() {
        anyhow::bail!("--fields must name at least one gh field");
    }
    let argv = build_argv(args, &fields.join(","));

    let argv_refs: Vec<&str> = argv.iter().map(String::as_str).collect();
    let stdout = client::invoke_ok("release", Some("view"), &argv_refs)?;

    render::emit_single_to_stdout(&stdout, &fields, args.format, args.limit)
}

/// Assemble the `gh release view` arg vector. The optional tag is positional
/// and must precede `--json`. Split out so it can be unit-tested without
/// spawning `gh`.
fn build_argv(args: &ReleaseViewArgs, fields_csv: &str) -> Vec<String> {
    let mut argv: Vec<String> = Vec::new();
    if let Some(tag) = &args.tag {
        argv.push(tag.clone());
    }
    argv.push("--json".into());
    argv.push(fields_csv.to_string());
    if let Some(repo) = &args.repo {
        argv.push("--repo".into());
        argv.push(repo.clone());
    }
    argv
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bare() -> ReleaseViewArgs {
        ReleaseViewArgs {
            tag: None,
            repo: None,
            fields: DEFAULT_FIELDS.into(),
            format: Format::Json,
            limit: None,
        }
    }

    #[test]
    fn no_tag_omits_positional() {
        let argv = build_argv(&bare(), DEFAULT_FIELDS);
        assert_eq!(argv, vec!["--json", DEFAULT_FIELDS]);
    }

    #[test]
    fn tag_precedes_json_flag() {
        let mut args = bare();
        args.tag = Some("v2.92.0".into());
        let argv = build_argv(&args, DEFAULT_FIELDS);
        assert_eq!(argv, vec!["v2.92.0", "--json", DEFAULT_FIELDS]);
    }

    #[test]
    fn repo_flag_follows_json() {
        let mut args = bare();
        args.tag = Some("v2.92.0".into());
        args.repo = Some("cli/cli".into());
        let argv = build_argv(&args, DEFAULT_FIELDS);
        assert_eq!(
            argv,
            vec!["v2.92.0", "--json", DEFAULT_FIELDS, "--repo", "cli/cli"]
        );
    }

    #[test]
    fn custom_fields_are_forwarded_verbatim() {
        let args = bare();
        let fields = render::parse_fields("tagName, name ,assets");
        let argv = build_argv(&args, &fields.join(","));
        assert_eq!(argv[1], "tagName,name,assets");
    }

    #[test]
    fn default_format_is_json() {
        assert_eq!(bare().format, Format::Json);
    }
}
