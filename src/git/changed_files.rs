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
        D(eleted), R(enamed, new path shown), plus T(ypechange) and C(opied).",
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

    // An unresolvable ref here is almost always a typo (same contract as
    // `diff`): a tool error, not a "no results" lookup.
    let old = resolve_to_tree(&repo, &args.ref1)?;
    let new = resolve_to_tree(&repo, &args.ref2)?;

    let diff = repo.diff_tree_to_tree(Some(&old), Some(&new), None)?;

    let mut rows: Vec<(String, String)> = Vec::new();
    for delta in diff.deltas() {
        let status = match delta.status() {
            git2::Delta::Added => "A",
            git2::Delta::Deleted => "D",
            git2::Delta::Modified => "M",
            git2::Delta::Renamed => "R",
            git2::Delta::Typechange => "T",
            git2::Delta::Copied => "C",
            // Tree-to-tree diffs only produce the statuses above; anything
            // else (Unmodified, Ignored, Untracked, Unreadable, Conflicted)
            // cannot occur here.
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
    if rows.is_empty() {
        return Ok(Outcome::NotFound);
    }
    rows.sort_by(|a, b| a.1.cmp(&b.1).then(a.0.cmp(&b.0)));

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

        let args = ChangedFilesArgs {
            repo: Some(repo.workdir().unwrap().to_path_buf()),
            ref1: "HEAD~1".to_string(),
            ref2: "HEAD".to_string(),
            limit: None,
        };
        let result = run(&args).unwrap();
        assert_eq!(result, Outcome::Found);
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
