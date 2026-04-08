use std::io;
use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::Result;
use clap::Args;
use git2::{Status, StatusOptions};

use crate::output::BoundedWriter;

#[derive(Args)]
#[command(
    about = "Show working tree status",
    long_about = "Show working tree status — staged, unstaged, and untracked files.\n\n\
        Outputs status in a porcelain-like format with two-character status codes \
        for index and working tree changes.",
    after_help = "\
Examples:
  sak git status                    Show all changes
  sak git status -C /path/to/repo   Status for another repo"
)]
pub struct StatusArgs {
    /// Path to the git repository
    #[arg(short = 'C', long)]
    pub repo: Option<PathBuf>,

    /// Maximum output lines
    #[arg(long)]
    pub limit: Option<usize>,
}

pub fn run(args: &StatusArgs) -> Result<ExitCode> {
    let repo = super::open_repo(&args.repo)?;

    let mut opts = StatusOptions::new();
    opts.include_untracked(true)
        .recurse_untracked_dirs(true)
        .include_ignored(false);

    let statuses = repo.statuses(Some(&mut opts))?;

    if statuses.is_empty() {
        return Ok(ExitCode::from(1));
    }

    // Collect and sort by path for determinism
    let mut entries: Vec<(String, String)> = Vec::new();
    for entry in statuses.iter() {
        let path = entry.path().unwrap_or("(invalid utf-8)");
        let status = entry.status();
        let code = format_status(status);
        entries.push((path.to_string(), code));
    }
    entries.sort_by(|a, b| a.0.cmp(&b.0));

    let stdout = io::stdout();
    let handle = stdout.lock();
    let mut writer = BoundedWriter::new(handle, args.limit);

    for (path, code) in &entries {
        if !writer.write_line(&format!("{} {}", code, path))? {
            break;
        }
    }
    writer.flush()?;

    Ok(ExitCode::SUCCESS)
}

fn format_status(status: Status) -> String {
    let index = if status.is_index_new() {
        'A'
    } else if status.is_index_modified() {
        'M'
    } else if status.is_index_deleted() {
        'D'
    } else if status.is_index_renamed() {
        'R'
    } else if status.is_index_typechange() {
        'T'
    } else {
        '.'
    };

    let wt = if status.is_wt_new() {
        '?'
    } else if status.is_wt_modified() {
        'M'
    } else if status.is_wt_deleted() {
        'D'
    } else if status.is_wt_renamed() {
        'R'
    } else if status.is_wt_typechange() {
        'T'
    } else {
        '.'
    };

    format!("{}{}", index, wt)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn test_status_clean_repo() {
        let (_dir, repo) = crate::git::init_test_repo();

        let args = StatusArgs {
            repo: Some(repo.workdir().unwrap().to_path_buf()),
            limit: None,
        };
        let result = run(&args).unwrap();
        assert_eq!(result, ExitCode::from(1));
    }

    #[test]
    fn test_status_with_new_file() {
        let (dir, repo) = crate::git::init_test_repo();
        fs::write(dir.path().join("new.txt"), "hello").unwrap();

        let args = StatusArgs {
            repo: Some(repo.workdir().unwrap().to_path_buf()),
            limit: None,
        };
        let result = run(&args).unwrap();
        assert_eq!(result, ExitCode::SUCCESS);
    }

    #[test]
    fn test_format_status_new_in_workdir() {
        let status = Status::WT_NEW;
        assert_eq!(format_status(status), ".?");
    }

    #[test]
    fn test_format_status_staged() {
        let status = Status::INDEX_NEW;
        assert_eq!(format_status(status), "A.");
    }
}
