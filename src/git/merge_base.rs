use crate::output::Outcome;
use std::io;
use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::Args;

use crate::output::BoundedWriter;

#[derive(Args)]
#[command(
    about = "Find the common ancestor of two refs",
    long_about = "Find the merge base (best common ancestor) of two refs.\n\n\
        Useful for reasoning about branch relationships: the merge base is \
        the fork point a branch diverged from.",
    after_help = "\
Examples:
  sak git merge-base main feature/my-branch     Fork point of a branch
  sak git merge-base HEAD~10 HEAD               Ancestor (here: HEAD~10 itself)"
)]
pub struct MergeBaseArgs {
    /// Path to the git repository
    #[arg(short = 'C', long)]
    pub repo: Option<PathBuf>,

    /// First ref (branch, tag, or revision)
    pub ref1: String,

    /// Second ref (branch, tag, or revision)
    pub ref2: String,
}

pub fn run(args: &MergeBaseArgs) -> Result<Outcome> {
    let repo = super::open_repo(&args.repo)?;

    // An unresolvable ref here is almost always a typo (same contract as
    // `diff` and `blame`): a tool error, not a "no results" lookup.
    let oid1 = resolve_to_commit(&repo, &args.ref1)?;
    let oid2 = resolve_to_commit(&repo, &args.ref2)?;

    let base_oid = repo
        .merge_base(oid1, oid2)
        .with_context(|| format!("no common ancestor of '{}' and '{}'", args.ref1, args.ref2))?;
    let base = repo.find_commit(base_oid)?;

    let stdout = io::stdout();
    let handle = stdout.lock();
    let mut writer = BoundedWriter::new(handle, None);

    let summary = base.summary().unwrap_or("");
    if summary.is_empty() {
        writer.write_line(&base_oid.to_string())?;
    } else {
        writer.write_line(&format!("{base_oid} {summary}"))?;
    }
    writer.flush()?;
    Ok(Outcome::Found)
}

/// Resolve a user-supplied ref to a commit OID, or bail with a message
/// naming the offending ref. Shared with `ahead-behind`, which resolves the
/// same way.
pub(crate) fn resolve_to_commit(repo: &git2::Repository, reference: &str) -> Result<git2::Oid> {
    let obj = repo
        .revparse_single(reference)
        .with_context(|| format!("cannot resolve '{reference}'"))?;
    Ok(obj
        .peel_to_commit()
        .with_context(|| format!("'{reference}' is not a commit"))?
        .id())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_merge_base_linear_history() {
        let (_dir, repo) = crate::git::init_test_repo();
        crate::git::create_test_commit(&repo, "first");
        crate::git::create_test_commit(&repo, "second");

        let args = MergeBaseArgs {
            repo: Some(repo.workdir().unwrap().to_path_buf()),
            ref1: "HEAD~1".to_string(),
            ref2: "HEAD".to_string(),
        };
        let result = run(&args).unwrap();
        assert_eq!(result, Outcome::Found);
    }

    #[test]
    fn test_merge_base_bad_ref_is_error() {
        let (_dir, repo) = crate::git::init_test_repo();
        crate::git::create_test_commit(&repo, "first");

        let args = MergeBaseArgs {
            repo: Some(repo.workdir().unwrap().to_path_buf()),
            ref1: "HEAD".to_string(),
            ref2: "deadbeef".to_string(),
        };
        let result = run(&args);
        assert!(result.is_err());
    }
}
