use crate::output::Outcome;
use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};

use anyhow::Result;
use clap::Args;

use crate::output::BoundedWriter;

#[derive(Args)]
#[command(
    about = "Commit count per file across history",
    long_about = "Hotspot detection for code review: commit count per file \
        across history, most-changed first. Each commit's diff against its \
        first parent contributes one count per touched file; merge commits \
        are skipped unless `--include-merges`.\n\n\
        `--since` bounds the walk to recent history; positional paths filter \
        to a subtree.",
    after_help = "\
Examples:
  sak git churn                               Whole-tree hotspots
  sak git churn src/                          Hotspots under src/ only
  sak git churn --since 2026-01-01            Changes since a date
  sak git churn --include-merges              Count merge diffs too"
)]
pub struct ChurnArgs {
    /// Path to the git repository
    #[arg(short = 'C', long)]
    pub repo: Option<PathBuf>,

    /// Only count commits on or after this date (YYYY-MM-DD)
    #[arg(long, value_name = "DATE")]
    pub since: Option<String>,

    /// Include merge commits (diffed against their first parent)
    #[arg(long)]
    pub include_merges: bool,

    /// Only count files under these paths
    pub paths: Vec<PathBuf>,

    /// Maximum number of output lines
    #[arg(long)]
    pub limit: Option<usize>,
}

/// Count commits touching each file, most-changed first (ties break by
/// path). Separated from [`run`] so tests can assert the counts instead of
/// only the exit [`Outcome`].
pub(crate) fn churn_counts(
    repo: &git2::Repository,
    include_merges: bool,
    since: Option<i64>,
    filters: &[PathBuf],
    workdir: Option<&Path>,
) -> Result<Vec<(PathBuf, u64)>> {
    let filters: Vec<PathBuf> = filters.iter().map(|f| relativize(workdir, f)).collect();
    let mut counts: HashMap<PathBuf, u64> = HashMap::new();
    super::walk::for_each_file_delta(repo, include_merges, since, |_commit, path| {
        if !filters.is_empty() && !filters.iter().any(|f| path.starts_with(f)) {
            return true;
        }
        *counts.entry(path.to_path_buf()).or_default() += 1;
        true
    })?;
    let mut out: Vec<(PathBuf, u64)> = counts.into_iter().collect();
    out.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    Ok(out)
}

/// Render a filter path repo-relative: absolute filters lose the workdir
/// prefix, relative ones pass through. Delta paths are repo-relative, so an
/// unrelativized absolute filter could never match.
pub(crate) fn relativize(workdir: Option<&Path>, filter: &Path) -> PathBuf {
    if filter.is_absolute()
        && let Some(wd) = workdir
        && let Ok(rel) = filter.strip_prefix(wd)
    {
        return rel.to_path_buf();
    }
    filter.to_path_buf()
}

pub fn run(args: &ChurnArgs) -> Result<Outcome> {
    let repo = super::open_repo(&args.repo)?;
    let since = super::contributors::parse_date_to_epoch(&args.since)?;
    let workdir = repo.workdir().map(Path::to_path_buf);
    let counts = churn_counts(
        &repo,
        args.include_merges,
        since,
        &args.paths,
        workdir.as_deref(),
    )?;
    if counts.is_empty() {
        return Ok(Outcome::NotFound);
    }

    let stdout = io::stdout();
    let handle = stdout.lock();
    let mut writer = BoundedWriter::new(handle, args.limit);
    for (path, count) in &counts {
        if !writer.write_line(&format!("{count}\t{}", path.display()))? {
            break;
        }
    }
    writer.flush()?;
    Ok(Outcome::Found)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo_with_history() -> (tempfile::TempDir, git2::Repository) {
        let (dir, repo) = crate::git::init_test_repo();
        crate::git::commit_file(&repo, "hot.txt", "v1", "hot 1", 1000);
        crate::git::commit_file(&repo, "hot.txt", "v2", "hot 2", 2000);
        crate::git::commit_file(&repo, "hot.txt", "v3", "hot 3", 3000);
        crate::git::commit_file(&repo, "cold.txt", "v1", "cold 1", 4000);
        (dir, repo)
    }

    #[test]
    fn counts_sort_most_changed_first() {
        let (_dir, repo) = repo_with_history();
        let counts = churn_counts(&repo, false, None, &[], repo.workdir()).unwrap();
        let names: Vec<(String, u64)> = counts
            .into_iter()
            .map(|(p, c)| (p.display().to_string(), c))
            .collect();
        assert_eq!(
            names,
            vec![("hot.txt".to_string(), 3), ("cold.txt".to_string(), 1),]
        );
    }

    #[test]
    fn subtree_filter_limits_counts() {
        let (_dir, repo) = crate::git::init_test_repo();
        crate::git::commit_file(&repo, "hot.txt", "v1", "hot 1", 1000);
        crate::git::commit_file(&repo, "src/a.txt", "v1", "src a", 2000);
        let counts =
            churn_counts(&repo, false, None, &[PathBuf::from("src")], repo.workdir()).unwrap();
        let names: Vec<String> = counts
            .into_iter()
            .map(|(p, _)| p.display().to_string())
            .collect();
        assert_eq!(names, vec!["src/a.txt".to_string()]);
    }

    #[test]
    fn merges_skipped_by_default_counted_with_flag() {
        let (_dir, repo) = crate::git::init_test_repo();
        crate::git::commit_file(&repo, "base.txt", "v1", "base", 1000);
        let a = repo.head().unwrap().peel_to_commit().unwrap();
        repo.branch("feature", &a, false).unwrap();
        crate::git::commit_file_on(&repo, "feat.txt", "v1", "feat", 2000, "refs/heads/feature");
        // Two-parent merge commit, fast-forward content: its diff against
        // the first parent re-shows feat.txt.
        let b = repo
            .revparse_single("refs/heads/feature")
            .unwrap()
            .peel_to_commit()
            .unwrap();
        let sig = git2::Signature::new("Test User", "test@example.com", &git2::Time::new(3000, 0))
            .unwrap();
        repo.commit(
            Some("HEAD"),
            &sig,
            &sig,
            "merge",
            &b.tree().unwrap(),
            &[&a, &b],
        )
        .unwrap();

        let plain = churn_counts(&repo, false, None, &[], repo.workdir()).unwrap();
        let names: Vec<(String, u64)> = plain
            .into_iter()
            .map(|(p, c)| (p.display().to_string(), c))
            .collect();
        assert_eq!(
            names,
            vec![("base.txt".to_string(), 1), ("feat.txt".to_string(), 1),]
        );
        let with_merges = churn_counts(&repo, true, None, &[], repo.workdir()).unwrap();
        let names: Vec<(String, u64)> = with_merges
            .into_iter()
            .map(|(p, c)| (p.display().to_string(), c))
            .collect();
        assert_eq!(
            names,
            vec![("feat.txt".to_string(), 2), ("base.txt".to_string(), 1),]
        );
    }

    #[test]
    fn empty_repo_is_not_found() {
        let (_dir, repo) = crate::git::init_test_repo();
        let args = ChurnArgs {
            repo: Some(repo.workdir().unwrap().to_path_buf()),
            since: None,
            include_merges: false,
            paths: vec![],
            limit: None,
        };
        assert_eq!(run(&args).unwrap(), Outcome::NotFound);
    }
}
