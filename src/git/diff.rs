use std::io;
use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{Context, Result};
use clap::Args;
use git2::{Diff, DiffFormat, DiffOptions};

use crate::output::BoundedWriter;

#[derive(Args)]
#[command(
    about = "Show file diffs",
    long_about = "Show file diffs between index, working directory, and commits.\n\n\
        Supports staged vs unstaged, commit ranges, and path filtering.",
    after_help = "\
Examples:
  sak git diff                       Unstaged changes
  sak git diff --staged              Staged changes
  sak git diff --name-only           Changed file names only
  sak git diff HEAD~3                Changes since 3 commits ago
  sak git diff HEAD~3 HEAD           Changes between two commits
  sak git diff -- src/               Changes in src/ only"
)]
pub struct DiffArgs {
    /// Path to the git repository
    #[arg(short = 'C', long)]
    pub repo: Option<PathBuf>,

    /// Show staged changes (index vs HEAD)
    #[arg(short = 's', long)]
    pub staged: bool,

    /// Show only file names that changed
    #[arg(long)]
    pub name_only: bool,

    /// Show stat summary instead of full diff
    #[arg(long)]
    pub stat: bool,

    /// First commit ref (compare this against working dir or second commit)
    #[arg(long)]
    pub commit: Option<String>,

    /// Second commit ref (compare commit..commit2)
    #[arg(long)]
    pub commit2: Option<String>,

    /// Paths to restrict the diff to
    #[arg(last = true)]
    pub paths: Vec<PathBuf>,

    /// Maximum output lines
    #[arg(long)]
    pub limit: Option<usize>,
}

pub fn run(args: &DiffArgs) -> Result<ExitCode> {
    let repo = super::open_repo(&args.repo)?;

    let mut opts = DiffOptions::new();
    for path in &args.paths {
        opts.pathspec(path);
    }

    let diff = build_diff(&repo, args, &mut opts)?;

    if diff.deltas().len() == 0 {
        return Ok(ExitCode::from(1));
    }

    let stdout = io::stdout();
    let handle = stdout.lock();
    let mut writer = BoundedWriter::new(handle, args.limit);

    if args.name_only {
        let mut names: Vec<String> = Vec::new();
        for delta in diff.deltas() {
            let path = delta
                .new_file()
                .path()
                .or_else(|| delta.old_file().path())
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_default();
            names.push(path);
        }
        names.sort();
        names.dedup();
        for name in &names {
            if !writer.write_line(name)? {
                break;
            }
        }
    } else if args.stat {
        let stats = diff.stats()?;
        let buf = stats.to_buf(git2::DiffStatsFormat::FULL, 80)?;
        let text = buf.as_str().unwrap_or("");
        for line in text.lines() {
            if !writer.write_line(line)? {
                break;
            }
        }
    } else {
        diff.print(DiffFormat::Patch, |_delta, _hunk, line| {
            let prefix = match line.origin() {
                '+' => "+",
                '-' => "-",
                ' ' => " ",
                _ => "",
            };
            let content = std::str::from_utf8(line.content()).unwrap_or("");
            let text = format!("{}{}", prefix, content.trim_end_matches('\n'));
            // Ignore write errors in callback (BoundedWriter handles truncation)
            let _ = writer.write_line(&text);
            true
        })?;
    }

    writer.flush()?;
    Ok(ExitCode::SUCCESS)
}

fn build_diff<'a>(
    repo: &'a git2::Repository,
    args: &DiffArgs,
    opts: &mut DiffOptions,
) -> Result<Diff<'a>> {
    if let Some(ref spec1) = args.commit {
        let obj1 = repo
            .revparse_single(spec1)
            .with_context(|| format!("cannot resolve '{}'", spec1))?;
        let tree1 = obj1.peel_to_tree()?;

        if let Some(ref spec2) = args.commit2 {
            let obj2 = repo
                .revparse_single(spec2)
                .with_context(|| format!("cannot resolve '{}'", spec2))?;
            let tree2 = obj2.peel_to_tree()?;
            Ok(repo.diff_tree_to_tree(Some(&tree1), Some(&tree2), Some(opts))?)
        } else {
            Ok(repo.diff_tree_to_workdir(Some(&tree1), Some(opts))?)
        }
    } else if args.staged {
        let head = repo.head()?.peel_to_tree()?;
        let index = repo.index()?;
        Ok(repo.diff_tree_to_index(Some(&head), Some(&index), Some(opts))?)
    } else {
        Ok(repo.diff_index_to_workdir(None, Some(opts))?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn create_initial_commit(repo: &git2::Repository) {
        let dir = repo.workdir().unwrap();
        fs::write(dir.join("init.txt"), "init").unwrap();
        let mut index = repo.index().unwrap();
        index.add_path(std::path::Path::new("init.txt")).unwrap();
        index.write().unwrap();
        let tree_oid = index.write_tree().unwrap();
        let tree = repo.find_tree(tree_oid).unwrap();
        let sig = repo.signature().unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, "initial commit", &tree, &[])
            .unwrap();
    }

    #[test]
    fn test_diff_no_changes() {
        let (_dir, repo) = crate::git::init_test_repo();
        create_initial_commit(&repo);

        let args = DiffArgs {
            repo: Some(repo.workdir().unwrap().to_path_buf()),
            staged: false,
            name_only: false,
            stat: false,
            commit: None,
            commit2: None,
            paths: vec![],
            limit: None,
        };
        let result = run(&args).unwrap();
        assert_eq!(result, ExitCode::from(1));
    }

    #[test]
    fn test_diff_unstaged_changes() {
        let (dir, repo) = crate::git::init_test_repo();
        create_initial_commit(&repo);
        fs::write(dir.path().join("init.txt"), "modified").unwrap();

        let args = DiffArgs {
            repo: Some(repo.workdir().unwrap().to_path_buf()),
            staged: false,
            name_only: false,
            stat: false,
            commit: None,
            commit2: None,
            paths: vec![],
            limit: None,
        };
        let result = run(&args).unwrap();
        assert_eq!(result, ExitCode::SUCCESS);
    }

    #[test]
    fn test_diff_name_only() {
        let (dir, repo) = crate::git::init_test_repo();
        create_initial_commit(&repo);
        fs::write(dir.path().join("init.txt"), "modified").unwrap();

        let args = DiffArgs {
            repo: Some(repo.workdir().unwrap().to_path_buf()),
            staged: false,
            name_only: true,
            stat: false,
            commit: None,
            commit2: None,
            paths: vec![],
            limit: None,
        };
        let result = run(&args).unwrap();
        assert_eq!(result, ExitCode::SUCCESS);
    }
}
