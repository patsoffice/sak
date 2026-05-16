pub mod headers;
pub mod validate;

use std::process::ExitCode;

use anyhow::Result;
use clap::Subcommand;

#[derive(Subcommand)]
pub enum CsvCommand {
    Headers(headers::HeadersArgs),
    Validate(validate::ValidateArgs),
}

pub fn run(cmd: &CsvCommand) -> Result<ExitCode> {
    match cmd {
        CsvCommand::Headers(args) => headers::run(args),
        CsvCommand::Validate(args) => validate::run(args),
    }
}
