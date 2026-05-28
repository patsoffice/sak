use crate::output::Outcome;
use std::io;

use anyhow::Result;
use clap::Args;

use crate::gh::argv::ArgvBuilder;
use crate::gh::client;
use crate::gh::render::{self, Format};
use crate::output::BoundedWriter;

/// Default `gh` field set for metadata mode when `--fields` is omitted.
/// Forwarded verbatim to `gh run view --json`; sak does not invent its own
/// field names. Validated against `gh run view --json` (with no value, gh
/// lists the accepted fields).
const DEFAULT_FIELDS: &str =
    "databaseId,workflowName,headBranch,event,status,conclusion,jobs,createdAt,updatedAt";

#[derive(Args)]
#[command(
    about = "Show a single workflow run, or its logs (read-only)",
    long_about = "Inspect a single GitHub Actions run via `gh run view <run-id>`. \
        Two modes:\n\n\
        Metadata (default): `gh run view <run-id> --json <fields>` emits the \
        run's status, conclusion, branch, event, and jobs. Output defaults to \
        `--format json` (the `jobs` array doesn't flatten cleanly into a \
        table); `--format tsv` emits one `field<TAB>value` line per requested \
        field.\n\n\
        Logs: `--log` streams the run's full logs; `--log-failed` streams only \
        the logs of failed steps. In log mode the raw text is streamed through \
        sak's bounded writer (so `--limit` truncates cleanly) and `--fields` / \
        `--format` are ignored. `--job <id>` narrows either mode to a single \
        job.\n\n\
        The `--fields` value is forwarded verbatim to `gh` — sak does not \
        maintain its own field-name set, so any column `gh run view --json` \
        accepts works here. Repository, auth, and host resolution are whatever \
        `gh` itself uses (the current directory's remote unless `--repo` is \
        given; `GH_TOKEN` / `GITHUB_TOKEN` or `~/.config/gh/hosts.yml`).",
    after_help = "\
Examples:
  sak gh run-view 26189297400                        Run metadata, JSON
  sak gh run-view 26189297400 --repo cli/cli --format tsv
  sak gh run-view 26189297400 --log-failed            Only failed-step logs
  sak gh run-view 26189297400 --job 123456 --log      Full logs for one job
  sak gh run-view 26189297400 --fields status,conclusion,jobs"
)]
pub struct RunViewArgs {
    /// Run database ID (the `databaseId` column from `sak gh run-list`)
    #[arg(value_name = "RUN-ID")]
    pub run_id: String,

    /// Repository in `owner/name` form (default: current directory's remote)
    #[arg(long, value_name = "OWNER/NAME")]
    pub repo: Option<String>,

    /// Stream the run's full logs instead of metadata
    #[arg(long)]
    pub log: bool,

    /// Stream only the logs of failed steps
    #[arg(long)]
    pub log_failed: bool,

    /// Narrow to a single job by ID (applies to both metadata and log modes)
    #[arg(long, value_name = "JOB-ID")]
    pub job: Option<String>,

    /// Comma-separated `gh` field names to request and project (metadata mode)
    #[arg(long, default_value = DEFAULT_FIELDS)]
    pub fields: String,

    /// Output format (metadata mode only; ignored with --log / --log-failed)
    #[arg(long, value_enum, default_value_t = Format::Json)]
    pub format: Format,

    /// Maximum number of output lines
    #[arg(long)]
    pub limit: Option<usize>,
}

impl RunViewArgs {
    /// `--log` / `--log-failed` switch from metadata to raw-log streaming.
    fn log_mode(&self) -> bool {
        self.log || self.log_failed
    }
}

pub fn run(args: &RunViewArgs) -> Result<Outcome> {
    if args.log_mode() {
        let argv = build_argv(args, "");
        let argv_refs: Vec<&str> = argv.iter().map(String::as_str).collect();
        let stdout = client::invoke_ok("run", Some("view"), &argv_refs)?;
        return emit_log(&stdout, args.limit);
    }

    let fields = render::parse_fields(&args.fields);
    if fields.is_empty() {
        anyhow::bail!("--fields must name at least one gh field");
    }
    let argv = build_argv(args, &fields.join(","));
    let argv_refs: Vec<&str> = argv.iter().map(String::as_str).collect();
    let stdout = client::invoke_ok("run", Some("view"), &argv_refs)?;

    render::emit_single_to_stdout(&stdout, &fields, args.format, args.limit)
}

/// Assemble the `gh run view` arg vector. The run ID is positional and leads;
/// `--json` (metadata) and `--log`/`--log-failed` (logs) are mutually
/// exclusive in `gh`, so only one is emitted. Split out for unit testing.
fn build_argv(args: &RunViewArgs, fields_csv: &str) -> Vec<String> {
    let mut b = ArgvBuilder::new();
    b.push_value(args.run_id.as_str())
        .push_opt("--repo", args.repo.as_deref())
        .push_opt("--job", args.job.as_deref());
    if args.log_mode() {
        b.push_flag_if(args.log, "--log")
            .push_flag_if(args.log_failed, "--log-failed");
    } else {
        b.push("--json", fields_csv);
    }
    b.into_argv()
}

/// Stream raw log text through the bounded writer, one line at a time, so
/// `--limit` truncates cleanly. Empty output maps to sak's exit code 1.
fn emit_log(stdout: &[u8], limit: Option<usize>) -> Result<Outcome> {
    let text = String::from_utf8_lossy(stdout);
    if text.trim().is_empty() {
        return Ok(Outcome::NotFound);
    }
    let out = io::stdout();
    let handle = out.lock();
    let mut writer = BoundedWriter::new(handle, limit);
    for line in text.split_inclusive('\n') {
        let line = line.strip_suffix('\n').unwrap_or(line);
        if !writer.write_line(line)? {
            break;
        }
    }
    writer.flush()?;
    Ok(Outcome::Found)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bare(run_id: &str) -> RunViewArgs {
        RunViewArgs {
            run_id: run_id.into(),
            repo: None,
            log: false,
            log_failed: false,
            job: None,
            fields: DEFAULT_FIELDS.into(),
            format: Format::Json,
            limit: None,
        }
    }

    #[test]
    fn metadata_mode_requests_json_fields() {
        let argv = build_argv(&bare("123"), DEFAULT_FIELDS);
        assert_eq!(argv, vec!["123", "--json", DEFAULT_FIELDS]);
    }

    #[test]
    fn log_mode_omits_json_and_passes_log_flag() {
        let mut args = bare("123");
        args.log = true;
        let argv = build_argv(&args, "");
        assert_eq!(argv, vec!["123", "--log"]);
        assert!(!argv.iter().any(|a| a == "--json"));
    }

    #[test]
    fn log_failed_mode_passes_log_failed_flag() {
        let mut args = bare("123");
        args.log_failed = true;
        let argv = build_argv(&args, "");
        assert_eq!(argv, vec!["123", "--log-failed"]);
    }

    #[test]
    fn repo_and_job_precede_mode_flag() {
        let mut args = bare("123");
        args.repo = Some("cli/cli".into());
        args.job = Some("999".into());
        args.log = true;
        let argv = build_argv(&args, "");
        assert_eq!(
            argv,
            vec!["123", "--repo", "cli/cli", "--job", "999", "--log"]
        );
    }

    #[test]
    fn job_in_metadata_mode_still_requests_json() {
        let mut args = bare("123");
        args.job = Some("999".into());
        let argv = build_argv(&args, DEFAULT_FIELDS);
        assert_eq!(argv, vec!["123", "--job", "999", "--json", DEFAULT_FIELDS]);
    }

    #[test]
    fn custom_fields_are_forwarded_verbatim() {
        let args = bare("123");
        let fields = render::parse_fields("status, conclusion ,jobs");
        let argv = build_argv(&args, &fields.join(","));
        assert_eq!(argv[2], "status,conclusion,jobs");
    }
}
