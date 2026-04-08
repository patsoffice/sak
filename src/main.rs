mod fs;
mod git;
mod output;

use std::process::ExitCode;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "sak",
    version,
    about = "Swiss Army Knife for LLMs — read-only operations",
    long_about = "Swiss Army Knife for LLMs — a collection of read-only operations \
        designed for use by language models.\n\n\
        All operations are strictly read-only with no side effects. \
        Commands are organized by domain (e.g., fs for filesystem). \
        Use `sak <domain> --help` to explore available operations, \
        or `sak <domain> <command> --help` for detailed usage.",
    after_help = "\
Quick start:
  sak fs glob '**/*.rs'                  Find all Rust files
  sak fs grep 'fn main' src/             Search for a pattern
  sak fs read src/main.rs -n 1-20        Read lines 1-20 of a file
  sak fs cut -d: -f 1 /etc/passwd        Extract first field
  sak git status                          Show working tree status
  sak git log --oneline -n 10             Recent commits
  sak git diff --staged                   Show staged changes
  sak git blame src/main.rs               Line-by-line authorship"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Filesystem operations (read-only)
    #[command(subcommand)]
    Fs(fs::FsCommand),
    /// Git repository operations (read-only)
    #[command(subcommand)]
    Git(git::GitCommand),
}

fn main() -> ExitCode {
    let cli = Cli::parse();

    let result = match &cli.command {
        Command::Fs(cmd) => fs::run(cmd),
        Command::Git(cmd) => git::run(cmd),
    };

    match result {
        Ok(code) => code,
        Err(e) => {
            eprintln!("sak: error: {:#}", e);
            ExitCode::from(2)
        }
    }
}
