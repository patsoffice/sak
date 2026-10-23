use crate::output::Outcome;
use std::io;
use std::path::PathBuf;

use anyhow::Result;
use clap::Args;

use crate::output::BoundedWriter;

#[derive(Args)]
#[command(
    about = "List tracked files",
    long_about = "List files tracked by git, one path per line, sorted.\n\n\
        Answers \"what does git track here?\" For untracked files use \
        `sak git status`; for content search use `sak fs grep`.",
    after_help = "\
Examples:
  sak git ls-files                  All tracked files, sorted
  sak git ls-files --limit 50       First 50 tracked files"
)]
pub struct LsFilesArgs {
    /// Path to the git repository
    #[arg(short = 'C', long)]
    pub repo: Option<PathBuf>,

    /// Maximum output lines
    #[arg(long)]
    pub limit: Option<usize>,
}

pub fn run(args: &LsFilesArgs) -> Result<Outcome> {
    let repo = super::open_repo(&args.repo)?;
    let index = repo.index()?;

    let mut paths: Vec<String> = Vec::new();
    for entry in index.iter() {
        paths.push(String::from_utf8_lossy(&entry.path).into_owned());
    }
    if paths.is_empty() {
        return Ok(Outcome::NotFound);
    }
    paths.sort();

    let stdout = io::stdout();
    let handle = stdout.lock();
    let mut writer = BoundedWriter::new(handle, args.limit);

    for path in &paths {
        if !writer.write_line(path)? {
            break;
        }
    }
    writer.flush()?;
    Ok(Outcome::Found)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ls_files_lists_committed_file() {
        let (_dir, repo) = crate::git::init_test_repo();
        crate::git::create_test_commit(&repo, "tracked content");

        let args = LsFilesArgs {
            repo: Some(repo.workdir().unwrap().to_path_buf()),
            limit: None,
        };
        let result = run(&args).unwrap();
        assert_eq!(result, Outcome::Found);
    }

    #[test]
    fn test_ls_files_empty_index_is_not_found() {
        let (_dir, repo) = crate::git::init_test_repo();

        let args = LsFilesArgs {
            repo: Some(repo.workdir().unwrap().to_path_buf()),
            limit: None,
        };
        let result = run(&args).unwrap();
        assert_eq!(result, Outcome::NotFound);
    }
}
