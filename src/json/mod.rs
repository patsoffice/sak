pub mod flatten;
pub mod keys;
pub mod query;
pub mod schema;
pub mod validate;

use std::io::{self, Read};
use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{Context, Result};
use clap::Subcommand;
use serde_json::Value;

#[derive(Subcommand)]
pub enum JsonCommand {
    Query(query::QueryArgs),
    Keys(keys::KeysArgs),
    Flatten(flatten::FlattenArgs),
    Schema(schema::SchemaArgs),
    Validate(validate::ValidateArgs),
}

pub fn run(cmd: &JsonCommand) -> Result<ExitCode> {
    match cmd {
        JsonCommand::Query(args) => query::run(args),
        JsonCommand::Keys(args) => keys::run(args),
        JsonCommand::Flatten(args) => flatten::run(args),
        JsonCommand::Schema(args) => schema::run(args),
        JsonCommand::Validate(args) => validate::run(args),
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
