//! Shared revwalk-with-diff traversal for history-aggregating commands.
//!
//! `churn` and `age` both walk HEAD's history and inspect each commit's delta
//! against its first parent; this module owns that traversal so the two
//! commands can't drift apart on merge, bound, or empty-history handling.

use anyhow::Result;
use std::path::Path;

/// Visit every commit newest-first, invoking `f` with the commit and each of
/// its file deltas against its first parent (against the empty tree for a
/// root commit). Merge commits are skipped unless `include_merges` (when
/// included they diff against their first parent, like any other commit).
/// Return `false` from `f` to stop the walk early. Stops at the `since` bound
/// (unix seconds, commits older than it end the walk). An empty repo (no
/// HEAD) visits nothing instead of erroring.
pub fn for_each_file_delta(
    repo: &git2::Repository,
    include_merges: bool,
    since: Option<i64>,
    mut f: impl FnMut(&git2::Commit, &Path) -> bool,
) -> Result<()> {
    let mut walker = repo.revwalk()?;
    // TIME ordering is load-bearing for `age`: first-seen-wins only yields
    // the newest touch per file when the walk is newest-first.
    walker.set_sorting(git2::Sort::TIME)?;
    if walker.push_head().is_err() {
        return Ok(());
    }
    for oid in walker {
        let oid = oid?;
        let commit = repo.find_commit(oid)?;
        if let Some(s) = since
            && commit.time().seconds() < s
        {
            break;
        }
        let parents: Vec<git2::Commit> = commit.parents().collect();
        if parents.len() > 1 && !include_merges {
            continue;
        }
        let parent_tree = if parents.is_empty() {
            None
        } else {
            Some(parents[0].tree()?)
        };
        let diff = repo.diff_tree_to_tree(parent_tree.as_ref(), Some(&commit.tree()?), None)?;
        for delta in diff.deltas() {
            let path = delta.new_file().path().or_else(|| delta.old_file().path());
            if let Some(p) = path
                && !f(&commit, p)
            {
                return Ok(());
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_repo_visits_nothing() {
        let (_dir, repo) = crate::git::init_test_repo();
        let mut seen = 0;
        for_each_file_delta(&repo, false, None, |_commit, _path| {
            seen += 1;
            true
        })
        .unwrap();
        assert_eq!(seen, 0);
    }

    #[test]
    fn root_commit_diffs_empty_tree() {
        let (_dir, repo) = crate::git::init_test_repo();
        crate::git::commit_file(&repo, "a.txt", "a", "add a", 1000);
        let mut paths = Vec::new();
        for_each_file_delta(&repo, false, None, |_commit, path| {
            paths.push(path.to_path_buf());
            true
        })
        .unwrap();
        assert_eq!(paths, vec![std::path::PathBuf::from("a.txt")]);
    }

    #[test]
    fn second_commit_diffs_first_parent_only() {
        let (_dir, repo) = crate::git::init_test_repo();
        crate::git::commit_file(&repo, "a.txt", "a", "add a", 1000);
        crate::git::commit_file(&repo, "b.txt", "b", "add b", 2000);
        let mut paths = Vec::new();
        for_each_file_delta(&repo, false, None, |_commit, path| {
            paths.push(path.to_path_buf());
            true
        })
        .unwrap();
        // Newest first: b.txt (second commit), then a.txt (root).
        let names: Vec<String> = paths.iter().map(|p| p.display().to_string()).collect();
        assert_eq!(names, vec!["b.txt".to_string(), "a.txt".to_string()]);
    }

    #[test]
    fn since_bound_stops_walk() {
        let (_dir, repo) = crate::git::init_test_repo();
        crate::git::commit_file(&repo, "a.txt", "a", "add a", 1000);
        crate::git::commit_file(&repo, "b.txt", "b", "add b", 2000);
        let mut paths = Vec::new();
        for_each_file_delta(&repo, false, Some(1500), |_commit, path| {
            paths.push(path.to_path_buf());
            true
        })
        .unwrap();
        assert_eq!(paths, vec![std::path::PathBuf::from("b.txt")]);
    }

    #[test]
    fn false_stops_walk() {
        let (_dir, repo) = crate::git::init_test_repo();
        crate::git::commit_file(&repo, "a.txt", "a", "add a", 1000);
        crate::git::commit_file(&repo, "b.txt", "b", "add b", 2000);
        let mut seen = 0;
        for_each_file_delta(&repo, false, None, |_commit, _path| {
            seen += 1;
            false
        })
        .unwrap();
        assert_eq!(seen, 1);
    }
}
