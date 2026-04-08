use std::io;
use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::Result;
use clap::Args;

use crate::output::BoundedWriter;

#[derive(Args)]
#[command(
    about = "List stash entries",
    long_about = "List stash entries with their index and message.\n\n\
        Output format matches git stash list: stash@{N}: message",
    after_help = "\
Examples:
  sak git stash-list                List all stash entries"
)]
pub struct StashArgs {
    /// Path to the git repository
    #[arg(short = 'C', long)]
    pub repo: Option<PathBuf>,

    /// Maximum output lines
    #[arg(long)]
    pub limit: Option<usize>,
}

pub fn run(args: &StashArgs) -> Result<ExitCode> {
    let mut repo = super::open_repo(&args.repo)?;

    let mut entries: Vec<(usize, String)> = Vec::new();
    repo.stash_foreach(|index, message, _oid| {
        entries.push((index, message.to_string()));
        true
    })?;

    if entries.is_empty() {
        return Ok(ExitCode::from(1));
    }

    let stdout = io::stdout();
    let handle = stdout.lock();
    let mut writer = BoundedWriter::new(handle, args.limit);

    for (index, message) in &entries {
        if !writer.write_line(&format!("stash@{{{}}}: {}", index, message))? {
            break;
        }
    }
    writer.flush()?;

    Ok(ExitCode::SUCCESS)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_stash_empty() {
        let (_dir, repo) = crate::git::init_test_repo();
        let args = StashArgs {
            repo: Some(repo.workdir().unwrap().to_path_buf()),
            limit: None,
        };
        let result = run(&args).unwrap();
        assert_eq!(result, ExitCode::from(1));
    }
}
