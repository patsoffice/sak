//! `nix` domain — read-only inspection of the Nix store, flakes, profiles,
//! and the registry.
//!
//! This repo dogfoods Nix via `flake.nix` + `.envrc`, and `nix` is the
//! standard "what's on this host?" tool for store paths, flake outputs,
//! locked revisions, profile contents, and derivation closures. This domain
//! wraps the read-only slice of `nix` with two pieces of value the raw
//! invocation lacks:
//!
//! 1. **Read-only enforcement** — `nix` has plenty of mutating subcommands
//!    (`build`, `copy`, `store delete`, `profile install`, `flake update`,
//!    ...). The chokepoint in [`client`] refuses to invoke any (verb, subverb)
//!    pair not on its allowlist, injects the `nix-command flakes` experimental
//!    features so the domain works on stock nix, and forces `--read-only` on
//!    `eval`. A grep test forbids `Command::new` / `"nix"` outside
//!    `client.rs`. Stronger than convention: even a well-meaning future
//!    contributor cannot accidentally land a path that pokes a mutating verb.
//! 2. **LLM-shaped output** — structured (`--json` passthrough) or TSV
//!    projections, mirroring the rest of sak.
//!
//! `sak nix` shells out to the system `nix` binary rather than re-implementing
//! the store/flake protocols; the cost of that decision is one external
//! runtime dependency in exchange for not tracking the upstream evaluator. If
//! `nix` isn't on PATH, the chokepoint returns a clear error. One command
//! (`references`) additionally shells out to the separate `nix-store` binary —
//! reverse dependencies (`--referrers`) have no modern `nix` subcommand — and
//! that path is gated by its own read-only sub-flag allowlist in [`client`].
//!
//! Individual commands (`flake-show`, `flake-metadata`, `store-info`,
//! `path-info`, `eval`, `registry-list`, `profile-list`, `derivation`,
//! `references`, `why-depends`) land as their own child issues and wire
//! themselves into [`NixCommand`] as they arrive.

pub mod client;
pub mod derivation_show;
pub mod eval;
pub mod flake_metadata;
pub mod flake_show;
pub mod path_info;
pub mod profile_list;
pub mod references;
pub mod registry_list;
pub mod store_info;

use std::process::ExitCode;

use anyhow::Result;
use clap::{Subcommand, ValueEnum};
use serde_json::Value;

use crate::output::{BoundedWriter, collapse_ws};

#[derive(Subcommand)]
pub enum NixCommand {
    FlakeShow(flake_show::FlakeShowArgs),
    StoreInfo(store_info::StoreInfoArgs),
    Eval(eval::EvalArgs),
    RegistryList(registry_list::RegistryListArgs),
    ProfileList(profile_list::ProfileListArgs),
    References(references::ReferencesArgs),
    DerivationShow(derivation_show::DerivationShowArgs),
    PathInfo(path_info::PathInfoArgs),
    FlakeMetadata(flake_metadata::FlakeMetadataArgs),
}

pub fn run(cmd: &NixCommand) -> Result<ExitCode> {
    match cmd {
        NixCommand::FlakeShow(args) => flake_show::run(args),
        NixCommand::StoreInfo(args) => store_info::run(args),
        NixCommand::Eval(args) => eval::run(args),
        NixCommand::RegistryList(args) => registry_list::run(args),
        NixCommand::ProfileList(args) => profile_list::run(args),
        NixCommand::References(args) => references::run(args),
        NixCommand::DerivationShow(args) => derivation_show::run(args),
        NixCommand::PathInfo(args) => path_info::run(args),
        NixCommand::FlakeMetadata(args) => flake_metadata::run(args),
    }
}

/// Render one `nix … --json` field to a TSV cell, shared by every nix command
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

/// Output format shared by the nix commands that wrap a `nix <cmd> --json`
/// invocation: a TSV projection (the default) or nix's JSON payload forwarded
/// verbatim.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum Format {
    Tsv,
    Json,
}

/// Lock stdout, emit per `format`, flush, and map present/empty to sak's 0/1
/// exit codes. `Json` streams nix's payload verbatim (a body equal to
/// `json_empty_marker` — e.g. `{}` or `[]` — counts as "no results"); `Tsv`
/// defers to the per-command `tsv` projection. Centralizes the stdout-locking
/// and exit-code contract so every nix command agrees on it.
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
