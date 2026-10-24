use crate::output::Outcome;
use std::collections::{HashMap, HashSet};
use std::io;
use std::path::{Path, PathBuf};

use anyhow::Result;
use clap::Args;

use crate::output::BoundedWriter;

#[derive(Args)]
#[command(
    about = "Last-touched timestamp per file",
    long_about = "Dead-code triage: last-touched unix timestamp and age in days \
        per file in HEAD, oldest first (deleted files don't appear). The walk \
        records each file's newest touch and stops once every file has one, \
        so old files never pay for a \
        full-history scan.\n\n\
        `--since` bounds the walk to recent history (files untouched in range \
        don't appear); positional paths filter to a subtree.",
    after_help = "\
Examples:
  sak git age                                 Oldest files first
  sak git age src/                            Oldest files under src/
  sak git age --since 2026-01-01              Files touched since a date"
)]
pub struct AgeArgs {
    /// Path to the git repository
    #[arg(short = 'C', long)]
    pub repo: Option<PathBuf>,

    /// Only consider commits on or after this date (YYYY-MM-DD)
    #[arg(long, value_name = "DATE")]
    pub since: Option<String>,

    /// Include merge commits when looking for the newest touch
    #[arg(long)]
    pub include_merges: bool,

    /// Only report files under these paths
    pub paths: Vec<PathBuf>,

    /// Maximum number of output lines
    #[arg(long)]
    pub limit: Option<usize>,
}

/// Last touch per file as `(path, timestamp, days_ago)`, oldest first (ties
/// break by path). `now_secs` is injected so tests can pin the clock.
/// Separated from [`run`] so tests can assert the triples instead of only
/// the exit [`Outcome`].
pub(crate) fn file_ages(
    repo: &git2::Repository,
    include_merges: bool,
    since: Option<i64>,
    filters: &[PathBuf],
    now_secs: i64,
) -> Result<Vec<(PathBuf, i64, i64)>> {
    let workdir = repo.workdir().map(Path::to_path_buf);
    let rel = |p: &Path| super::churn::relativize(workdir.as_deref(), p);
    let filters: Vec<PathBuf> = filters.iter().map(|f| rel(f)).collect();

    // Completion set: the files in HEAD's tree under the filters. HEAD, not
    // the index: a bare repo has no index, and a staged-only file has no
    // history to find. An empty set (no commits, or no filter matches) has
    // nothing to report, so skip the walk.
    let mut remaining: HashSet<PathBuf> = head_files(repo)?
        .into_iter()
        .filter(|p| filters.is_empty() || filters.iter().any(|f| p.starts_with(f)))
        .collect();
    if remaining.is_empty() {
        return Ok(Vec::new());
    }

    // Only files still in HEAD count: a deleted file's last delta is its
    // deletion. Removing a path on its first (newest, since the walk is
    // newest-first) touch keeps that touch and ignores older ones.
    let mut touched: HashMap<PathBuf, i64> = HashMap::new();
    super::walk::for_each_file_delta(repo, include_merges, since, |commit, path| {
        if remaining.remove(path) {
            touched.insert(path.to_path_buf(), commit.time().seconds());
        }
        !remaining.is_empty()
    })?;

    let mut out: Vec<(PathBuf, i64, i64)> = touched
        .into_iter()
        .map(|(path, ts)| {
            let days_ago = (now_secs - ts).max(0) / 86400;
            (path, ts, days_ago)
        })
        .collect();
    out.sort_by(|a, b| a.1.cmp(&b.1).then(a.0.cmp(&b.0)));
    Ok(out)
}

/// Every non-directory entry (files, symlinks, submodules) in HEAD's tree,
/// repo-relative. An unborn HEAD has none.
fn head_files(repo: &git2::Repository) -> Result<Vec<PathBuf>> {
    let Some(tree) = repo.head().ok().and_then(|h| h.peel_to_tree().ok()) else {
        return Ok(Vec::new());
    };
    let mut files = Vec::new();
    tree.walk(git2::TreeWalkMode::PreOrder, |root, entry| {
        if entry.kind() != Some(git2::ObjectType::Tree) {
            let name = String::from_utf8_lossy(entry.name_bytes());
            files.push(PathBuf::from(format!("{root}{name}")));
        }
        git2::TreeWalkResult::Ok
    })?;
    Ok(files)
}

pub fn run(args: &AgeArgs) -> Result<Outcome> {
    let repo = super::open_repo(&args.repo)?;
    let since = super::contributors::parse_date_to_epoch(&args.since)?;
    let now_secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let ages = file_ages(&repo, args.include_merges, since, &args.paths, now_secs)?;
    if ages.is_empty() {
        return Ok(Outcome::NotFound);
    }

    let stdout = io::stdout();
    let handle = stdout.lock();
    let mut writer = BoundedWriter::new(handle, args.limit);
    for (path, timestamp, days_ago) in &ages {
        if !writer.write_line(&format!("{timestamp}\t{days_ago}\t{}", path.display()))? {
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
    fn oldest_first_with_exact_triples() {
        let (_dir, repo) = crate::git::init_test_repo();
        crate::git::commit_file(&repo, "old.txt", "v1", "old", 1000);
        crate::git::commit_file(&repo, "new.txt", "v1", "new", 2000);
        // Retouch old.txt: its last touch is the newest commit.
        crate::git::commit_file(&repo, "old.txt", "v2", "old again", 3000);
        let ages = file_ages(&repo, false, None, &[], 3200).unwrap();
        let rows: Vec<(String, i64, i64)> = ages
            .into_iter()
            .map(|(p, ts, days)| (p.display().to_string(), ts, days))
            .collect();
        assert_eq!(
            rows,
            vec![
                ("new.txt".to_string(), 2000, 0),
                ("old.txt".to_string(), 3000, 0),
            ]
        );
    }

    #[test]
    fn days_ago_counts_whole_days() {
        let (_dir, repo) = crate::git::init_test_repo();
        crate::git::commit_file(&repo, "a.txt", "v1", "a", 1000);
        let ages = file_ages(&repo, false, None, &[], 1000 + 3 * 86400 + 10).unwrap();
        assert_eq!(ages.len(), 1);
        assert_eq!(ages[0].2, 3);
    }

    /// Commit the removal of `name` at `time_secs` on HEAD.
    fn commit_delete(repo: &git2::Repository, name: &str, time_secs: i64) {
        std::fs::remove_file(repo.workdir().unwrap().join(name)).unwrap();
        let mut index = repo.index().unwrap();
        index.remove_path(Path::new(name)).unwrap();
        index.write().unwrap();
        let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
        let sig = git2::Signature::new(
            "Test User",
            "test@example.com",
            &git2::Time::new(time_secs, 0),
        )
        .unwrap();
        let parent = repo.head().unwrap().peel_to_commit().unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, "delete", &tree, &[&parent])
            .unwrap();
    }

    fn names(ages: Vec<(PathBuf, i64, i64)>) -> Vec<String> {
        ages.into_iter()
            .map(|(p, _, _)| p.display().to_string())
            .collect()
    }

    #[test]
    fn deleted_files_are_not_reported() {
        // Regression: the deletion delta recorded `gone.txt` as the newest file.
        let (_dir, repo) = crate::git::init_test_repo();
        crate::git::commit_file(&repo, "keep.txt", "v1", "keep", 1000);
        crate::git::commit_file(&repo, "gone.txt", "v1", "gone", 2000);
        commit_delete(&repo, "gone.txt", 3000);
        assert_eq!(
            names(file_ages(&repo, false, None, &[], 4000).unwrap()),
            vec!["keep.txt".to_string()]
        );
    }

    #[test]
    fn tracked_set_comes_from_head_not_the_index() {
        // Regression: the index stood in for "tracked files", so a bare repo
        // (no index) stopped after one arbitrary delta and a staged-only file
        // kept the walk from ever finishing early.
        let (dir, repo) = crate::git::init_test_repo();
        crate::git::commit_file(&repo, "a.txt", "v1", "a", 1000);
        crate::git::commit_file(&repo, "src/b.txt", "v1", "b", 2000);
        std::fs::write(dir.path().join("staged.txt"), "s").unwrap();
        let mut index = repo.index().unwrap();
        index.add_path(Path::new("staged.txt")).unwrap();
        index.write().unwrap();
        let want = vec!["a.txt".to_string(), "src/b.txt".to_string()];
        assert_eq!(
            names(file_ages(&repo, false, None, &[], 3000).unwrap()),
            want
        );

        let bare_dir = tempfile::tempdir().unwrap();
        let bare = git2::build::RepoBuilder::new()
            .bare(true)
            .clone(dir.path().to_str().unwrap(), bare_dir.path())
            .unwrap();
        assert_eq!(
            names(file_ages(&bare, false, None, &[], 3000).unwrap()),
            want
        );
    }

    #[test]
    fn unmatched_filter_reports_nothing() {
        let (_dir, repo) = crate::git::init_test_repo();
        crate::git::commit_file(&repo, "a.txt", "v1", "a", 1000);
        let ages = file_ages(&repo, false, None, &[PathBuf::from("nomatch")], 2000).unwrap();
        assert!(ages.is_empty());
    }

    #[test]
    fn empty_repo_is_not_found() {
        let (_dir, repo) = crate::git::init_test_repo();
        let args = AgeArgs {
            repo: Some(repo.workdir().unwrap().to_path_buf()),
            since: None,
            include_merges: false,
            paths: vec![],
            limit: None,
        };
        assert_eq!(run(&args).unwrap(), Outcome::NotFound);
    }
}
