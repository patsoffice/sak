//! `helm` domain — read-only inspection of Helm releases, charts, and repos.
//!
//! Sibling to the [`crate::k8s`] domain: LLMs reach for `helm` constantly in
//! Kubernetes incident triage (what release state is this in? what was
//! actually rendered? what values are live?), and today that path bottoms out
//! at shelling to `helm` directly. This domain formalizes the read-only
//! subset behind a verb allowlist.
//!
//! Like [`crate::talos`] and the nix domain, `sak helm` shells out to the
//! system `helm` binary rather than re-implementing the release client; the
//! cost is one external runtime dependency in exchange for not tracking
//! Helm's storage backend and chart-rendering surface. If `helm` isn't on
//! PATH, the chokepoint returns a clear error.
//!
//! Read-only enforcement lives in [`client`]: a `(verb, subverb)` allowlist
//! refuses every mutating subcommand (`install`, `upgrade`, `uninstall`,
//! `repo add`, ...), and a grep test forbids `Command::new` / `"helm"`
//! outside `client.rs`.
//!
//! Individual commands (`get`, `status`, `history`, `show`, `template`,
//! `search`, `lint`, `repo-list`, `dependency-list`) land as their own child
//! issues and wire themselves into [`HelmCommand`] as they arrive.

pub mod client;
pub mod dependency_list;
pub mod get;
pub mod history;
pub mod lint;
pub mod list;
pub mod repo_list;
pub mod show;
pub mod status;
pub mod template;

use std::process::ExitCode;

use anyhow::Result;
use clap::{Subcommand, ValueEnum};
use serde_json::Value;

use crate::output::{BoundedWriter, collapse_ws};

#[derive(Subcommand)]
pub enum HelmCommand {
    List(list::ListArgs),
    Status(status::StatusArgs),
    Get(get::GetArgs),
    History(history::HistoryArgs),
    RepoList(repo_list::RepoListArgs),
    DependencyList(dependency_list::DependencyListArgs),
    Show(show::ShowArgs),
    Template(template::TemplateArgs),
    Lint(lint::LintArgs),
}

pub fn run(cmd: &HelmCommand) -> Result<ExitCode> {
    match cmd {
        HelmCommand::List(args) => list::run(args),
        HelmCommand::Status(args) => status::run(args),
        HelmCommand::Get(args) => get::run(args),
        HelmCommand::History(args) => history::run(args),
        HelmCommand::RepoList(args) => repo_list::run(args),
        HelmCommand::DependencyList(args) => dependency_list::run(args),
        HelmCommand::Show(args) => show::run(args),
        HelmCommand::Template(args) => template::run(args),
        HelmCommand::Lint(args) => lint::run(args),
    }
}

/// Render one `helm -o json` field to a TSV cell, shared by every helm command
/// that projects JSON into a fixed-column table. Missing / null → `-`; scalars
/// render verbatim with whitespace collapsed (so a value can never inject a tab
/// or newline into the row); anything structured falls back to compact JSON.
pub fn render_cell(v: Option<&Value>) -> String {
    match v {
        None | Some(Value::Null) => "-".to_string(),
        Some(Value::String(s)) => collapse_ws(s),
        Some(Value::Number(n)) => n.to_string(),
        Some(Value::Bool(b)) => b.to_string(),
        Some(other) => collapse_ws(&serde_json::to_string(other).unwrap_or_default()),
    }
}

/// Output format shared by the helm commands that wrap `helm <cmd> -o json`:
/// a TSV projection (the default, one or more fixed-column rows) or helm's
/// JSON payload forwarded verbatim.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum Format {
    Tsv,
    Json,
}

/// Lock stdout, emit per `format`, flush, and map present/empty to sak's 0/1
/// exit codes. `Json` streams helm's payload verbatim (a body equal to
/// `json_empty_marker` — `[]` or `{}` — counts as "no results"); `Tsv` defers
/// to the per-command `tsv` projection. Centralizes the stdout-locking and
/// exit-code contract so every helm command agrees on it.
pub fn emit_to_stdout(
    stdout: &[u8],
    format: Format,
    limit: Option<usize>,
    json_empty_marker: &str,
    tsv: impl FnOnce(&mut BoundedWriter<'_>, &[u8]) -> Result<bool>,
) -> Result<ExitCode> {
    let out = std::io::stdout();
    let mut writer = BoundedWriter::new(out.lock(), limit);
    let any = match format {
        Format::Json => stream_verbatim(&mut writer, stdout, json_empty_marker)?,
        Format::Tsv => tsv(&mut writer, stdout)?,
    };
    writer.flush()?;
    Ok(if any {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    })
}

/// Stream a raw text payload (e.g. `helm get manifest`'s YAML) to stdout
/// unchanged, mapping empty output to sak's exit code 1. Used by commands whose
/// output is helm's native text rather than a JSON projection — there is no
/// TSV/JSON choice, so this is a thin wrapper over [`stream_verbatim`] with an
/// empty marker of `""` (only truly empty output is "no results").
pub fn emit_text_to_stdout(stdout: &[u8], limit: Option<usize>) -> Result<ExitCode> {
    let out = std::io::stdout();
    let mut writer = BoundedWriter::new(out.lock(), limit);
    let any = stream_verbatim(&mut writer, stdout, "")?;
    writer.flush()?;
    Ok(if any {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    })
}

/// Stream a JSON body to the writer unchanged, line by line. An empty body or
/// one equal to `empty_marker` counts as "no results" (`Ok(false)`).
fn stream_verbatim(
    writer: &mut BoundedWriter<'_>,
    stdout: &[u8],
    empty_marker: &str,
) -> Result<bool> {
    let text = String::from_utf8_lossy(stdout);
    let trimmed = text.trim();
    if trimmed.is_empty() || trimmed == empty_marker {
        return Ok(false);
    }
    for line in text.split_inclusive('\n') {
        let line = line.strip_suffix('\n').unwrap_or(line);
        if !writer.write_line(line)? {
            break;
        }
    }
    Ok(true)
}
