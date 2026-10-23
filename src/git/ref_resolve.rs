use crate::output::Outcome;
use std::io;
use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::Args;

use crate::output::BoundedWriter;

#[derive(Args)]
#[command(
    about = "Resolve a ref to a commit SHA",
    long_about = "Resolve a ref (branch, tag, HEAD~3) to a commit SHA plus metadata.\n\n\
        Quick lookup without full show output: prints the peeled commit SHA,\n\
        the direct object type, and the commit subject.",
    after_help = "\
Examples:
  sak git ref-resolve HEAD           Resolve HEAD
  sak git ref-resolve main           Resolve a branch
  sak git ref-resolve v1.0           Resolve a tag to its commit"
)]
pub struct RefResolveArgs {
    /// Path to the git repository
    #[arg(short = 'C', long)]
    pub repo: Option<PathBuf>,

    /// Branch, tag, or revision to resolve (e.g. HEAD, main, v1.0, HEAD~3)
    pub reference: String,
}

pub fn run(args: &RefResolveArgs) -> Result<Outcome> {
    let repo = super::open_repo(&args.repo)?;

    let obj = match repo.revparse_single(&args.reference) {
        Ok(obj) => obj,
        Err(e) if e.code() == git2::ErrorCode::NotFound => return Ok(Outcome::NotFound),
        Err(e) => {
            return Err(e).with_context(|| format!("cannot resolve '{}'", args.reference));
        }
    };
    let kind = match obj.kind() {
        Some(git2::ObjectType::Commit) => "commit",
        Some(git2::ObjectType::Tag) => "tag",
        Some(git2::ObjectType::Tree) => "tree",
        Some(git2::ObjectType::Blob) => "blob",
        _ => "unknown",
    };

    // Peel annotated tags (and other indirections) to the commit, like
    // `git rev-parse <ref>^{commit}`. Blobs and trees have no commit to
    // peel to; report the object itself with no summary.
    let (sha, summary) = match obj.peel_to_commit() {
        Ok(commit) => (
            commit.id().to_string(),
            commit.summary().unwrap_or("").to_string(),
        ),
        Err(_) => (obj.id().to_string(), String::new()),
    };

    let stdout = io::stdout();
    let handle = stdout.lock();
    let mut writer = BoundedWriter::new(handle, None);

    if summary.is_empty() {
        writer.write_line(&format!("{sha} {kind}"))?;
    } else {
        writer.write_line(&format!("{sha} {kind} {summary}"))?;
    }
    writer.flush()?;
    Ok(Outcome::Found)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ref_resolve_head() {
        let (_dir, repo) = crate::git::init_test_repo();
        let oid = crate::git::create_test_commit(&repo, "test commit");

        let args = RefResolveArgs {
            repo: Some(repo.workdir().unwrap().to_path_buf()),
            reference: "HEAD".to_string(),
        };
        let result = run(&args).unwrap();
        assert_eq!(result, Outcome::Found);
        assert_eq!(oid.to_string().len(), 40);
    }

    #[test]
    fn test_ref_resolve_bad_ref_is_not_found() {
        let (_dir, repo) = crate::git::init_test_repo();

        let args = RefResolveArgs {
            repo: Some(repo.workdir().unwrap().to_path_buf()),
            reference: "deadbeef".to_string(),
        };
        let result = run(&args).unwrap();
        assert_eq!(result, Outcome::NotFound);
    }
}
