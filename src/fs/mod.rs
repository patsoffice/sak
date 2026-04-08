pub mod cut;
pub mod glob;
pub mod grep;
pub mod read;

use std::process::ExitCode;

use anyhow::Result;
use clap::Subcommand;

#[derive(Subcommand)]
pub enum FsCommand {
    Glob(glob::GlobArgs),
    Grep(grep::GrepArgs),
    Cut(cut::CutArgs),
    Read(read::ReadArgs),
}

pub fn run(cmd: &FsCommand) -> Result<ExitCode> {
    match cmd {
        FsCommand::Glob(args) => glob::run(args),
        FsCommand::Grep(args) => grep::run(args),
        FsCommand::Cut(args) => cut::run(args),
        FsCommand::Read(args) => read::run(args),
    }
}
