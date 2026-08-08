use crate::output::Outcome;
use std::io;
use std::path::PathBuf;

use anyhow::Result;
use clap::Args;
use git2::{Status, StatusOptions};

use crate::output::BoundedWriter;

#[derive(Args)]
#[command(
    about = "Show working tree status",
    long_about = "Show working tree status — staged, unstaged, and untracked files.\n\n\
        Outputs status in a porcelain-like format with two-character status codes \
        for index and working tree changes.\n\n\
        Pass paths after `--` to scope the report to a subtree, e.g. \
        `sak git status -- src bin` — useful for checking whether a generated \
        or vendored directory is tracked before touching it.",
    after_help = "\
Examples:
  sak git status                    Show all changes
  sak git status -- src             Only changes under src/
  sak git status -C /path/to/repo   Status for another repo"
)]
pub struct StatusArgs {
    /// Path to the git repository
    #[arg(short = 'C', long)]
    pub repo: Option<PathBuf>,

    /// Restrict the status report to these paths
    #[arg(last = true)]
    pub paths: Vec<PathBuf>,

    /// Maximum output lines
    #[arg(long)]
    pub limit: Option<usize>,
}

pub fn run(args: &StatusArgs) -> Result<Outcome> {
    let repo = super::open_repo(&args.repo)?;

    let mut opts = StatusOptions::new();
    opts.include_untracked(true)
        .recurse_untracked_dirs(true)
        .include_ignored(false);
    for path in &args.paths {
        opts.pathspec(path);
    }

    let statuses = repo.statuses(Some(&mut opts))?;

    if statuses.is_empty() {
        return Ok(Outcome::NotFound);
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

    Ok(Outcome::Found)
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
            paths: vec![],
            limit: None,
        };
        let result = run(&args).unwrap();
        assert_eq!(result, Outcome::NotFound);
    }

    #[test]
    fn test_status_with_new_file() {
        let (dir, repo) = crate::git::init_test_repo();
        fs::write(dir.path().join("new.txt"), "hello").unwrap();

        let args = StatusArgs {
            repo: Some(repo.workdir().unwrap().to_path_buf()),
            paths: vec![],
            limit: None,
        };
        let result = run(&args).unwrap();
        assert_eq!(result, Outcome::Found);
    }

    #[test]
    fn pathspec_scopes_the_report() {
        let (dir, repo) = crate::git::init_test_repo();
        fs::create_dir(dir.path().join("src")).unwrap();
        fs::write(dir.path().join("src/a.txt"), "a").unwrap();
        fs::write(dir.path().join("other.txt"), "b").unwrap();

        let scoped = StatusArgs {
            repo: Some(repo.workdir().unwrap().to_path_buf()),
            paths: vec![PathBuf::from("src")],
            limit: None,
        };
        assert_eq!(run(&scoped).unwrap(), Outcome::Found);

        // A pathspec matching nothing is a negative result, not an error.
        let empty = StatusArgs {
            paths: vec![PathBuf::from("no-such-dir")],
            ..scoped
        };
        assert_eq!(run(&empty).unwrap(), Outcome::NotFound);
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
