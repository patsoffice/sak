use crate::output::Outcome;
use std::io;
use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::Args;

use crate::output::BoundedWriter;

#[derive(Args)]
#[command(
    about = "List files changed between two refs",
    long_about = "List files changed between two refs, one per line as \
        <status> TAB <path>.\n\n\
        Simpler than `diff --name-only` when only the file list matters. \
        Status letters follow `git diff --name-status`: A(dded), M(odified), \
        D(eleted), T(ypechange), and R(enamed, new path shown), detected by \
        content similarity as git does by default. Copies are not detected; \
        a copied file shows as A.",
    after_help = "\
Examples:
  sak git changed-files HEAD~3            Files changed in the last 3 commits
  sak git changed-files main HEAD         Files differing from main
  sak git changed-files v1.0 v2.0         Files changed between releases"
)]
pub struct ChangedFilesArgs {
    /// Path to the git repository
    #[arg(short = 'C', long)]
    pub repo: Option<PathBuf>,

    /// Older ref (compared against ref2)
    pub ref1: String,

    /// Newer ref (default: HEAD)
    #[arg(default_value = "HEAD")]
    pub ref2: String,

    /// Maximum output lines
    #[arg(long)]
    pub limit: Option<usize>,
}

pub fn run(args: &ChangedFilesArgs) -> Result<Outcome> {
    let repo = super::open_repo(&args.repo)?;
    let rows = changed_rows(&repo, &args.ref1, &args.ref2)?;
    if rows.is_empty() {
        return Ok(Outcome::NotFound);
    }

    let stdout = io::stdout();
    let handle = stdout.lock();
    let mut writer = BoundedWriter::new(handle, args.limit);

    for (status, path) in &rows {
        if !writer.write_line(&format!("{status}\t{path}"))? {
            break;
        }
    }
    writer.flush()?;
    Ok(Outcome::Found)
}

/// `(status, path)` rows for the files that differ between two refs, sorted
/// by path. Split from [`run`] so the rows are testable without stdout.
fn changed_rows(repo: &git2::Repository, ref1: &str, ref2: &str) -> Result<Vec<(String, String)>> {
    // An unresolvable ref here is almost always a typo (same contract as
    // `diff`): a tool error, not a "no results" lookup.
    let old = resolve_to_tree(repo, ref1)?;
    let new = resolve_to_tree(repo, ref2)?;

    let mut diff = repo.diff_tree_to_tree(Some(&old), Some(&new), None)?;
    // libgit2 reports a rename as delete + add unless asked to pair them up;
    // git's own diff does rename detection by default, so match it.
    diff.find_similar(Some(git2::DiffFindOptions::new().renames(true)))?;

    let mut rows: Vec<(String, String)> = Vec::new();
    for delta in diff.deltas() {
        let status = match delta.status() {
            git2::Delta::Added => "A",
            git2::Delta::Deleted => "D",
            git2::Delta::Modified => "M",
            git2::Delta::Renamed => "R",
            git2::Delta::Typechange => "T",
            // Copy detection is off (see find_similar above), so no Copied
            // deltas; nor can a tree-to-tree diff produce Unmodified,
            // Ignored, Untracked, Unreadable, or Conflicted.
            _ => continue,
        };
        // Renames show the new path (the old one no longer exists).
        let path = delta
            .new_file()
            .path()
            .or_else(|| delta.old_file().path())
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default();
        rows.push((status.to_string(), path));
    }
    rows.sort_by(|a, b| a.1.cmp(&b.1).then(a.0.cmp(&b.0)));
    Ok(rows)
}

/// Resolve a user-supplied ref to its tree, or bail naming the ref.
fn resolve_to_tree<'a>(repo: &'a git2::Repository, reference: &str) -> Result<git2::Tree<'a>> {
    let tree_id = repo
        .revparse_single(reference)
        .with_context(|| format!("cannot resolve '{reference}'"))?
        .peel_to_tree()
        .with_context(|| format!("'{reference}' has no tree"))?
        .id();
    Ok(repo.find_tree(tree_id)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_changed_files_lists_added() {
        let (_dir, repo) = crate::git::init_test_repo();
        crate::git::create_test_commit(&repo, "first file");
        crate::git::create_test_commit(&repo, "second file");

        assert_eq!(
            changed_rows(&repo, "HEAD~1", "HEAD").unwrap(),
            [("A".to_string(), "second_file.txt".to_string())]
        );
    }

    #[test]
    fn test_changed_files_detects_renames() {
        // Regression: without find_similar a rename was D + A, though the
        // help promised R.
        let (_dir, repo) = crate::git::init_test_repo();
        let content = "a file with enough content to be recognisably the same\n".repeat(4);
        let dir = repo.workdir().unwrap().to_path_buf();
        std::fs::write(dir.join("old.txt"), &content).unwrap();
        commit_index(&repo, |index| {
            index.add_path(std::path::Path::new("old.txt")).unwrap()
        });
        std::fs::rename(dir.join("old.txt"), dir.join("new.txt")).unwrap();
        commit_index(&repo, |index| {
            index.remove_path(std::path::Path::new("old.txt")).unwrap();
            index.add_path(std::path::Path::new("new.txt")).unwrap();
        });

        assert_eq!(
            changed_rows(&repo, "HEAD~1", "HEAD").unwrap(),
            [("R".to_string(), "new.txt".to_string())]
        );
    }

    /// Apply `stage` to the index and commit it on HEAD.
    fn commit_index(repo: &git2::Repository, stage: impl FnOnce(&mut git2::Index)) {
        let mut index = repo.index().unwrap();
        stage(&mut index);
        index.write().unwrap();
        let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
        let sig = repo.signature().unwrap();
        let parents: Vec<git2::Commit> = repo
            .head()
            .ok()
            .and_then(|h| h.peel_to_commit().ok())
            .into_iter()
            .collect();
        let parent_refs: Vec<&git2::Commit> = parents.iter().collect();
        repo.commit(Some("HEAD"), &sig, &sig, "change", &tree, &parent_refs)
            .unwrap();
    }

    #[test]
    fn test_changed_files_identical_refs_is_not_found() {
        let (_dir, repo) = crate::git::init_test_repo();
        crate::git::create_test_commit(&repo, "first file");

        let args = ChangedFilesArgs {
            repo: Some(repo.workdir().unwrap().to_path_buf()),
            ref1: "HEAD".to_string(),
            ref2: "HEAD".to_string(),
            limit: None,
        };
        let result = run(&args).unwrap();
        assert_eq!(result, Outcome::NotFound);
    }

    #[test]
    fn test_changed_files_bad_ref_is_error() {
        let (_dir, repo) = crate::git::init_test_repo();
        crate::git::create_test_commit(&repo, "first file");

        let args = ChangedFilesArgs {
            repo: Some(repo.workdir().unwrap().to_path_buf()),
            ref1: "HEAD".to_string(),
            ref2: "deadbeef".to_string(),
            limit: None,
        };
        let result = run(&args);
        assert!(result.is_err());
    }
}
