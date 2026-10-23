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
    let paths = tracked_paths(&repo.index()?);
    if paths.is_empty() {
        return Ok(Outcome::NotFound);
    }

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

/// Sorted, de-duplicated index paths. During a merge conflict the index holds
/// one entry per stage (base, ours, theirs) for the same path; `git ls-files`
/// without `--stage` lists such a path once, and so does this.
fn tracked_paths(index: &git2::Index) -> Vec<String> {
    let mut paths: Vec<String> = index
        .iter()
        .map(|entry| String::from_utf8_lossy(&entry.path).into_owned())
        .collect();
    paths.sort();
    paths.dedup();
    paths
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ls_files_lists_committed_file() {
        let (_dir, repo) = crate::git::init_test_repo();
        crate::git::create_test_commit(&repo, "tracked content");
        assert_eq!(
            tracked_paths(&repo.index().unwrap()),
            ["tracked_content.txt"]
        );
    }

    #[test]
    fn test_ls_files_lists_a_conflicted_path_once() {
        // Regression: a merge conflict leaves stage 1/2/3 entries for one
        // path, which were each listed.
        let (_dir, repo) = crate::git::init_test_repo();
        let blob = repo.blob(b"conflicted\n").unwrap();
        let mut index = repo.index().unwrap();
        for stage in 1..=3u16 {
            let path = b"clash.txt".to_vec();
            index
                .add(&git2::IndexEntry {
                    ctime: git2::IndexTime::new(0, 0),
                    mtime: git2::IndexTime::new(0, 0),
                    dev: 0,
                    ino: 0,
                    mode: 0o100644,
                    uid: 0,
                    gid: 0,
                    file_size: 0,
                    id: blob,
                    // Stage lives in bits 12-13 of the flags, length below.
                    flags: (stage << 12) | path.len() as u16,
                    flags_extended: 0,
                    path,
                })
                .unwrap();
        }
        assert!(index.has_conflicts(), "fixture should be conflicted");
        assert_eq!(tracked_paths(&index), ["clash.txt"]);
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
