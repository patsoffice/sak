pub mod headers;

use std::process::ExitCode;

use anyhow::Result;
use clap::Subcommand;

#[derive(Subcommand)]
pub enum CsvCommand {
    Headers(headers::HeadersArgs),
}

pub fn run(cmd: &CsvCommand) -> Result<ExitCode> {
    match cmd {
        CsvCommand::Headers(args) => headers::run(args),
    }
}
