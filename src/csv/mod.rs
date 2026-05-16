pub mod headers;
pub mod query;
pub mod stats;
pub mod validate;

use std::process::ExitCode;

use anyhow::Result;
use clap::Subcommand;

#[derive(Subcommand)]
pub enum CsvCommand {
    Headers(headers::HeadersArgs),
    Query(query::QueryArgs),
    Stats(stats::StatsArgs),
    Validate(validate::ValidateArgs),
}

pub fn run(cmd: &CsvCommand) -> Result<ExitCode> {
    match cmd {
        CsvCommand::Headers(args) => headers::run(args),
        CsvCommand::Query(args) => query::run(args),
        CsvCommand::Stats(args) => stats::run(args),
        CsvCommand::Validate(args) => validate::run(args),
    }
}
