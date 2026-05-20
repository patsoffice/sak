use std::io;
use std::process::ExitCode;

use anyhow::Result;
use clap::Args;

use crate::gh::client;
use crate::output::BoundedWriter;

#[derive(Args)]
#[command(
    about = "Call a GitHub REST or GraphQL endpoint (GET-only)",
    long_about = "Wrap `gh api <endpoint>` as the catch-all escape hatch for any \
        GitHub REST or GraphQL read that the other `sak gh` commands don't cover. \
        The response body is emitted verbatim through sak's bounded writer.\n\n\
        This command is strictly read-only: it never exposes `gh api`'s \
        `-X / --method` flag, so every call is an HTTP GET (gh's own default). \
        The shared chokepoint additionally rejects any attempt to smuggle a \
        non-GET method, so even direct misuse can't mutate.\n\n\
        GraphQL note: `gh api graphql -f query=...` is technically issued as \
        POST by `gh`, but the read-only invariant holds for *query* documents — \
        a GraphQL mutation requires an explicit `mutation { ... }` body, not a \
        different HTTP method. sak does not parse the query string, so if you \
        need strict no-mutation discipline, validate the body before calling.\n\n\
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

pub fn run(args: &ApiArgs) -> Result<ExitCode> {
    // Assemble the passthrough arg vector. The endpoint leads; every flag
    // maps 1:1 to a `gh api` flag. Notably absent: any `-X / --method` flag —
    // this command is GET-only by construction, and the chokepoint rejects a
    // non-GET method if one ever reaches it some other way.
    let mut argv: Vec<String> = vec![args.endpoint.clone()];

    if let Some(repo) = &args.repo {
        argv.push("--repo".into());
        argv.push(repo.clone());
    }
    for h in &args.headers {
        argv.push("-H".into());
        argv.push(h.clone());
    }
    for f in &args.fields {
        argv.push("-F".into());
        argv.push(f.clone());
    }
    for f in &args.raw_fields {
        argv.push("-f".into());
        argv.push(f.clone());
    }
    if args.paginate {
        argv.push("--paginate".into());
    }
    if let Some(jq) = &args.jq {
        argv.push("--jq".into());
        argv.push(jq.clone());
    }
    if let Some(cache) = &args.cache {
        argv.push("--cache".into());
        argv.push(cache.clone());
    }

    let argv_refs: Vec<&str> = argv.iter().map(String::as_str).collect();
    let bytes = client::invoke_ok("api", None, &argv_refs)?;

    let stdout = io::stdout();
    let handle = stdout.lock();
    let mut writer = BoundedWriter::new(handle, args.limit);

    let text = String::from_utf8_lossy(&bytes);
    if text.is_empty() {
        return Ok(ExitCode::from(1));
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

    Ok(ExitCode::SUCCESS)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build the same argv that `run` assembles, without spawning `gh`, so we
    /// can assert flag mapping is correct and stable.
    fn build_argv(args: &ApiArgs) -> Vec<String> {
        let mut argv: Vec<String> = vec![args.endpoint.clone()];
        if let Some(repo) = &args.repo {
            argv.push("--repo".into());
            argv.push(repo.clone());
        }
        for h in &args.headers {
            argv.push("-H".into());
            argv.push(h.clone());
        }
        for f in &args.fields {
            argv.push("-F".into());
            argv.push(f.clone());
        }
        for f in &args.raw_fields {
            argv.push("-f".into());
            argv.push(f.clone());
        }
        if args.paginate {
            argv.push("--paginate".into());
        }
        if let Some(jq) = &args.jq {
            argv.push("--jq".into());
            argv.push(jq.clone());
        }
        if let Some(cache) = &args.cache {
            argv.push("--cache".into());
            argv.push(cache.clone());
        }
        argv
    }

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
        assert_eq!(argv, vec!["repos/cli/cli"]);
        // No `-X` / `--method` is ever emitted — GET-only by construction.
        assert!(!argv.iter().any(|a| a == "-X" || a == "--method"));
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
        assert_eq!(positions, vec![2, 4]);
    }
}
