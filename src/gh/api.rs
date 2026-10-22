use crate::output::Outcome;
use std::io;

use anyhow::Result;
use clap::Args;

use crate::gh::argv::ArgvBuilder;
use crate::gh::client;
use crate::output::BoundedWriter;

#[derive(Args)]
#[command(
    about = "Call a GitHub REST or GraphQL endpoint (GET-only)",
    long_about = "Wrap `gh api <endpoint>` as the catch-all escape hatch for any \
        GitHub REST or GraphQL read that the other `sak gh` commands don't cover. \
        The response body is emitted verbatim through sak's bounded writer.\n\n\
        This command is strictly read-only: it never exposes `gh api`'s \
        `-X / --method` flag and pins every REST call to `-X GET`, so \
        `--field` / `--raw-field` are sent as a query string (left to itself, \
        gh would switch to POST as soon as a parameter is added). The shared \
        chokepoint additionally rejects any non-GET method or implicit POST, \
        so even direct misuse can't mutate.\n\n\
        GraphQL: `sak gh api graphql --raw-field query=…` is POSTed by gh (it \
        only speaks POST to GraphQL), so the document is what keeps it a read. \
        The chokepoint scans every `query` document and rejects any that \
        defines a non-`query` operation (`mutation`, `subscription`), \
        including one hidden after a harmless first operation. A query it \
        can't inspect — read from a file with `--field query=@path`, or \
        malformed — is rejected too; pass the document inline with \
        `--raw-field`. Other fields are sent as GraphQL variables.\n\n\
        Auth, base URL, and host resolution are whatever `gh` itself uses \
        (`GH_TOKEN` / `GITHUB_TOKEN` or `~/.config/gh/hosts.yml`); sak passes \
        the environment through unchanged.",
    after_help = "\
Examples:
  sak gh api repos/cli/cli                          Repo metadata as JSON
  sak gh api repos/{owner}/{repo}/releases --repo cli/cli
  sak gh api 'repos/cli/cli/issues?state=open' --jq '.[].title'
  sak gh api repos/cli/cli/contributors --paginate  Follow pagination
  sak gh api user --jq .login                        Current authed login
  sak gh api graphql -f query='{ viewer { login } }'  GraphQL query"
)]
pub struct ApiArgs {
    /// Endpoint path (e.g. `repos/cli/cli`) or `graphql`
    pub endpoint: String,

    /// Repository in `owner/name` form, substituted into `{owner}`/`{repo}` placeholders
    #[arg(long, value_name = "OWNER/NAME")]
    pub repo: Option<String>,

    /// Request header `Key: Value` (repeatable), passed through as `gh -H`
    #[arg(long = "header", value_name = "KEY:VALUE")]
    pub headers: Vec<String>,

    /// Typed parameter `key=value` (repeatable), passed through as `gh -F`
    #[arg(long = "field", value_name = "KEY=VALUE")]
    pub fields: Vec<String>,

    /// String parameter `key=value` (repeatable), passed through as `gh -f`
    #[arg(long = "raw-field", value_name = "KEY=VALUE")]
    pub raw_fields: Vec<String>,

    /// Follow pagination, concatenating every page
    #[arg(long)]
    pub paginate: bool,

    /// Server-side jq filter applied by `gh`
    #[arg(long, value_name = "FILTER")]
    pub jq: Option<String>,

    /// Cache responses for the given duration (e.g. `30s`, `5m`) — a read optimization
    #[arg(long, value_name = "DURATION")]
    pub cache: Option<String>,

    /// Maximum number of output lines
    #[arg(long)]
    pub limit: Option<usize>,
}

pub fn run(args: &ApiArgs) -> Result<Outcome> {
    let argv = build_argv(args);
    let argv_refs: Vec<&str> = argv.iter().map(String::as_str).collect();
    let bytes = client::invoke_ok("api", None, &argv_refs)?;

    let stdout = io::stdout();
    let handle = stdout.lock();
    let mut writer = BoundedWriter::new(handle, args.limit);

    let text = String::from_utf8_lossy(&bytes);
    if text.is_empty() {
        return Ok(Outcome::NotFound);
    }
    for line in text.split_inclusive('\n') {
        // split_inclusive keeps the trailing '\n'; write_line re-adds one if
        // missing, so strip it here to avoid doubled newlines.
        let trimmed = line.strip_suffix('\n').unwrap_or(line);
        if !writer.write_line(trimmed)? {
            break;
        }
    }
    writer.flush()?;

    Ok(Outcome::Found)
}

/// Assemble the `gh api` passthrough arg vector. The endpoint leads; every flag
/// maps 1:1 to a `gh api` flag. REST endpoints are pinned to `-X GET`: gh
/// otherwise switches to POST as soon as `-f`/`-F` adds a parameter, and with
/// GET pinned those parameters go out as a query string instead. GraphQL is
/// left alone (gh only speaks POST to it). The chokepoint rejects any argv
/// that would still not be a read. Split out for unit testing.
fn build_argv(args: &ApiArgs) -> Vec<String> {
    let mut b = ArgvBuilder::new();
    b.push_value(args.endpoint.as_str());
    if args.endpoint != client::GRAPHQL_ENDPOINT {
        b.push("-X", "GET");
    }
    b.push_opt("--repo", args.repo.as_deref())
        .push_each("-H", &args.headers)
        .push_each("-F", &args.fields)
        .push_each("-f", &args.raw_fields)
        .push_flag_if(args.paginate, "--paginate")
        .push_opt("--jq", args.jq.as_deref())
        .push_opt("--cache", args.cache.as_deref());
    b.into_argv()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bare(endpoint: &str) -> ApiArgs {
        ApiArgs {
            endpoint: endpoint.into(),
            repo: None,
            headers: vec![],
            fields: vec![],
            raw_fields: vec![],
            paginate: false,
            jq: None,
            cache: None,
            limit: None,
        }
    }

    #[test]
    fn bare_endpoint_passes_only_the_endpoint() {
        let argv = build_argv(&bare("repos/cli/cli"));
        assert_eq!(argv, vec!["repos/cli/cli", "-X", "GET"]);
    }

    #[test]
    fn rest_fields_stay_a_get() {
        // Regression (sak-llm-16cp): without the pinned GET, gh POSTs as soon
        // as a field is present. The built argv must classify as a read.
        let mut args = bare("repos/o/r/issues");
        args.raw_fields = vec!["title=x".into()];
        args.fields = vec!["per_page=5".into()];
        let argv = build_argv(&args);
        assert_eq!(client::api_method(&argv), client::ApiMethod::Get);
    }

    #[test]
    fn graphql_is_not_pinned_to_get() {
        // gh only POSTs to GraphQL; forcing GET would break every query.
        let mut args = bare("graphql");
        args.raw_fields = vec!["query={ viewer { login } }".into()];
        let argv = build_argv(&args);
        assert!(!argv.iter().any(|a| a == "-X"), "{argv:?}");
        assert_eq!(client::api_method(&argv), client::ApiMethod::GraphQl);
    }

    #[test]
    fn flags_map_to_gh_spellings() {
        let mut args = bare("repos/{owner}/{repo}/issues");
        args.repo = Some("cli/cli".into());
        args.headers = vec!["Accept: application/vnd.github+json".into()];
        args.fields = vec!["per_page=5".into()];
        args.raw_fields = vec!["state=open".into()];
        args.paginate = true;
        args.jq = Some(".[].title".into());
        args.cache = Some("30s".into());

        let argv = build_argv(&args);
        assert_eq!(
            argv,
            vec![
                "repos/{owner}/{repo}/issues",
                "-X",
                "GET",
                "--repo",
                "cli/cli",
                "-H",
                "Accept: application/vnd.github+json",
                "-F",
                "per_page=5",
                "-f",
                "state=open",
                "--paginate",
                "--jq",
                ".[].title",
                "--cache",
                "30s",
            ]
        );
    }

    #[test]
    fn repeatable_flags_preserve_order() {
        let mut args = bare("user");
        args.headers = vec!["A: 1".into(), "B: 2".into()];
        let argv = build_argv(&args);
        let positions: Vec<usize> = argv
            .iter()
            .enumerate()
            .filter(|(_, a)| *a == "A: 1" || *a == "B: 2")
            .map(|(i, _)| i)
            .collect();
        assert_eq!(positions, vec![4, 6]);
    }
}
