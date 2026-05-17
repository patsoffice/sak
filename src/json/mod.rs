pub mod diff;
pub mod exists;
pub mod flatten;
pub mod grep;
pub mod keys;
pub mod length;
pub mod paths;
pub mod query;
pub mod schema;
pub mod type_;
pub mod validate;

use std::io::{self, BufRead, BufReader, Read};
use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{Context, Result};
use clap::Subcommand;
use serde_json::Value;

#[derive(Subcommand)]
pub enum JsonCommand {
    Query(query::QueryArgs),
    Exists(exists::ExistsArgs),
    Keys(keys::KeysArgs),
    Flatten(flatten::FlattenArgs),
    Grep(grep::GrepArgs),
    Length(length::LengthArgs),
    Paths(paths::PathsArgs),
    Schema(schema::SchemaArgs),
    Type(type_::TypeArgs),
    Validate(validate::ValidateArgs),
    Diff(diff::DiffArgs),
}

pub fn run(cmd: &JsonCommand) -> Result<ExitCode> {
    match cmd {
        JsonCommand::Query(args) => query::run(args),
        JsonCommand::Exists(args) => exists::run(args),
        JsonCommand::Keys(args) => keys::run(args),
        JsonCommand::Flatten(args) => flatten::run(args),
        JsonCommand::Grep(args) => grep::run(args),
        JsonCommand::Length(args) => length::run(args),
        JsonCommand::Paths(args) => paths::run(args),
        JsonCommand::Schema(args) => schema::run(args),
        JsonCommand::Type(args) => type_::run(args),
        JsonCommand::Validate(args) => validate::run(args),
        JsonCommand::Diff(args) => diff::run(args),
    }
}

/// Read JSON inputs from the given files, or from stdin if `files` is empty.
/// Returns a vector of `(source_name, value)` pairs.
pub fn read_json_inputs(files: &[PathBuf]) -> Result<Vec<(String, Value)>> {
    let mut out = Vec::new();
    if files.is_empty() {
        let mut s = String::new();
        io::stdin()
            .read_to_string(&mut s)
            .context("error reading stdin")?;
        let value: Value = serde_json::from_str(&s).context("invalid JSON on stdin")?;
        out.push(("<stdin>".to_string(), value));
    } else {
        for path in files {
            let s = std::fs::read_to_string(path)
                .with_context(|| format!("cannot read: {}", path.display()))?;
            let value: Value = serde_json::from_str(&s)
                .with_context(|| format!("invalid JSON: {}", path.display()))?;
            out.push((path.display().to_string(), value));
        }
    }
    Ok(out)
}

/// Read NDJSON (newline-delimited JSON) inputs — one JSON value per non-blank
/// line — from the given files, or from stdin if `files` is empty. Blank /
/// whitespace-only lines are skipped (the NDJSON convention). Each emitted
/// `source_name` is `"<file>:<lineno>"` so per-record context survives into
/// downstream output. The first parse error aborts with a contextual message.
pub fn read_json_inputs_lines(files: &[PathBuf]) -> Result<Vec<(String, Value)>> {
    let mut out = Vec::new();
    if files.is_empty() {
        let stdin = io::stdin();
        read_ndjson_into("<stdin>", stdin.lock(), &mut out)?;
    } else {
        for path in files {
            let file = std::fs::File::open(path)
                .with_context(|| format!("cannot read: {}", path.display()))?;
            read_ndjson_into(&path.display().to_string(), BufReader::new(file), &mut out)?;
        }
    }
    Ok(out)
}

/// Wrapper that picks between whole-document and NDJSON readers.
pub fn read_json_inputs_maybe_lines(
    files: &[PathBuf],
    lines: bool,
) -> Result<Vec<(String, Value)>> {
    if lines {
        read_json_inputs_lines(files)
    } else {
        read_json_inputs(files)
    }
}

fn read_ndjson_into<R: BufRead>(
    name: &str,
    reader: R,
    out: &mut Vec<(String, Value)>,
) -> Result<()> {
    for (idx, line) in reader.lines().enumerate() {
        let lineno = idx + 1;
        let line = line.with_context(|| format!("error reading {}", name))?;
        if line.trim().is_empty() {
            continue;
        }
        let value: Value = serde_json::from_str(&line)
            .with_context(|| format!("invalid JSON: {}:{}", name, lineno))?;
        out.push((format!("{}:{}", name, lineno), value));
    }
    Ok(())
}
