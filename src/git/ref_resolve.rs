use crate::output::Outcome;
use std::io;
use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::Args;

use crate::output::BoundedWriter;

#[derive(Args)]
#[command(
    about = "Resolve a ref to an object SHA and its commit",
    long_about = "Resolve a ref (branch, tag, HEAD~3) to an object SHA plus metadata.\n\n\
        Quick lookup without full show output: prints one line, \
        `<sha> <type> <commit> <subject>`. `<sha>` is exactly what \
        `git rev-parse <ref>` prints (for an annotated tag, the tag object \
        itself); `<type>` is that object's type; `<commit>` is the commit it \
        peels to (the same SHA for a commit, `-` for a tree or blob); \
        `<subject>` is that commit's subject.",
    after_help = "\
Examples:
  sak git ref-resolve HEAD           Resolve HEAD
  sak git ref-resolve main           Resolve a branch
  sak git ref-resolve v1.0           Tag object SHA plus the commit it points at"
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
    let stdout = io::stdout();
    let handle = stdout.lock();
    let mut writer = BoundedWriter::new(handle, None);
    writer.write_line(&describe(&obj))?;
    writer.flush()?;
    Ok(Outcome::Found)
}

/// `<sha> <type> <commit> <subject>` for a resolved object. `<sha>` is the
/// object itself — what `git rev-parse <ref>` prints, which the agent hook
/// redirects here, so the two must agree (an annotated tag's SHA, not the
/// commit's). `<commit>` is the peel, like `git rev-parse <ref>^{commit}`;
/// trees and blobs have none and show `-` with an empty subject.
fn describe(obj: &git2::Object) -> String {
    let kind = match obj.kind() {
        Some(git2::ObjectType::Commit) => "commit",
        Some(git2::ObjectType::Tag) => "tag",
        Some(git2::ObjectType::Tree) => "tree",
        Some(git2::ObjectType::Blob) => "blob",
        _ => "unknown",
    };
    match obj.peel_to_commit() {
        Ok(commit) => format!(
            "{} {kind} {} {}",
            obj.id(),
            commit.id(),
            commit.summary().unwrap_or("")
        )
        .trim_end()
        .to_string(),
        Err(_) => format!("{} {kind} -", obj.id()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ref_resolve_head() {
        let (_dir, repo) = crate::git::init_test_repo();
        let oid = crate::git::create_test_commit(&repo, "test commit");
        let obj = repo.revparse_single("HEAD").unwrap();
        assert_eq!(describe(&obj), format!("{oid} commit {oid} test commit"));
    }

    #[test]
    fn test_ref_resolve_annotated_tag_matches_rev_parse() {
        // Regression: the SHA was the peeled commit, so `git rev-parse v1.0`
        // (redirected here by the hook) and sak disagreed for annotated tags.
        let (_dir, repo) = crate::git::init_test_repo();
        let commit_oid = crate::git::create_test_commit(&repo, "release");
        let commit = repo.find_object(commit_oid, None).unwrap();
        let sig = repo.signature().unwrap();
        let tag_oid = repo
            .tag("v1.0", &commit, &sig, "v1.0 notes", false)
            .unwrap();

        let obj = repo.revparse_single("v1.0").unwrap();
        assert_eq!(obj.id(), tag_oid);
        assert_eq!(
            describe(&obj),
            format!("{tag_oid} tag {commit_oid} release")
        );
    }

    #[test]
    fn test_ref_resolve_tree_has_no_commit() {
        let (_dir, repo) = crate::git::init_test_repo();
        crate::git::create_test_commit(&repo, "tree source");
        let obj = repo.revparse_single("HEAD^{tree}").unwrap();
        assert_eq!(describe(&obj), format!("{} tree -", obj.id()));
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
