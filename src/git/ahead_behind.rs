use crate::output::Outcome;
use std::io;
use std::path::PathBuf;

use anyhow::Result;
use clap::Args;

use crate::output::BoundedWriter;

#[derive(Args)]
#[command(
    about = "Count commits each ref has that the other doesn't",
    long_about = "How many commits each ref has that the other doesn't: \
        `ahead` counts ref1-only commits, `behind` counts ref2-only commits.\n\n\
        One call to `Repository::graph_ahead_behind`; ref2 defaults to HEAD.",
    after_help = "\
Examples:
  sak git ahead-behind feature/my-branch          Branch vs HEAD (ref2 defaults to HEAD)
  sak git ahead-behind main feature/my-branch       Commits main has that the branch lacks"
)]
pub struct AheadBehindArgs {
    /// Path to the git repository
    #[arg(short = 'C', long)]
    pub repo: Option<PathBuf>,

    /// First ref (branch, tag, or revision)
    pub ref1: String,

    /// Second ref (branch, tag, or revision; default HEAD)
    #[arg(default_value = "HEAD")]
    pub ref2: String,
}

/// Resolve both refs and count each side's unique commits: `(ahead, behind)`
/// where ahead is ref1-only and behind is ref2-only. Separated from [`run`]
/// so tests can assert the counts instead of only the exit [`Outcome`].
pub(crate) fn ahead_behind_counts(
    repo: &git2::Repository,
    oid1: git2::Oid,
    oid2: git2::Oid,
) -> Result<(usize, usize)> {
    Ok(repo.graph_ahead_behind(oid1, oid2)?)
}

pub fn run(args: &AheadBehindArgs) -> Result<Outcome> {
    let repo = super::open_repo(&args.repo)?;

    // An unresolvable ref here is almost always a typo (same contract as
    // `diff` and `blame`): a tool error, not a "no results" lookup.
    let oid1 = super::merge_base::resolve_to_commit(&repo, &args.ref1)?;
    let oid2 = super::merge_base::resolve_to_commit(&repo, &args.ref2)?;
    let (ahead, behind) = ahead_behind_counts(&repo, oid1, oid2)?;

    let stdout = io::stdout();
    let handle = stdout.lock();
    let mut writer = BoundedWriter::new(handle, None);
    writer.write_line(&format!("ahead\t{ahead}"))?;
    writer.write_line(&format!("behind\t{behind}"))?;
    writer.flush()?;
    Ok(Outcome::Found)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo_with_branch() -> (tempfile::TempDir, git2::Repository, git2::Oid, git2::Oid) {
        let (dir, repo) = crate::git::init_test_repo();
        crate::git::commit_file(&repo, "a.txt", "a", "add a", 1000);
        let base = repo.head().unwrap().peel_to_commit().unwrap();
        repo.branch("feature", &base, false).unwrap();
        drop(base);
        crate::git::commit_file(&repo, "b.txt", "b", "add b", 2000);
        let main_oid = repo.revparse_single("HEAD").unwrap().id();
        let feature_oid = repo.revparse_single("feature").unwrap().id();
        (dir, repo, feature_oid, main_oid)
    }

    #[test]
    fn counts_each_sides_unique_commits() {
        let (_dir, repo, feature_oid, main_oid) = repo_with_branch();
        assert_eq!(
            ahead_behind_counts(&repo, feature_oid, main_oid).unwrap(),
            (0, 1)
        );
        assert_eq!(
            ahead_behind_counts(&repo, main_oid, feature_oid).unwrap(),
            (1, 0)
        );
        assert_eq!(
            ahead_behind_counts(&repo, main_oid, main_oid).unwrap(),
            (0, 0)
        );
    }

    #[test]
    fn run_emits_both_lines() {
        let (_dir, repo) = crate::git::init_test_repo();
        crate::git::commit_file(&repo, "a.txt", "a", "add a", 1000);
        let args = AheadBehindArgs {
            repo: Some(repo.workdir().unwrap().to_path_buf()),
            ref1: "HEAD".to_string(),
            ref2: "HEAD".to_string(),
        };
        assert_eq!(run(&args).unwrap(), Outcome::Found);
    }

    #[test]
    fn run_bad_ref_is_error() {
        let (_dir, repo) = crate::git::init_test_repo();
        crate::git::commit_file(&repo, "a.txt", "a", "add a", 1000);
        let args = AheadBehindArgs {
            repo: Some(repo.workdir().unwrap().to_path_buf()),
            ref1: "HEAD".to_string(),
            ref2: "deadbeef".to_string(),
        };
        assert!(run(&args).is_err());
    }
}
