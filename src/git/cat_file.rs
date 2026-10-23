use crate::output::Outcome;
use std::io::{self, Write};
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use clap::Args;

use crate::output::BoundedWriter;

#[derive(Args)]
#[command(
    about = "Dump a raw git object",
    long_about = "Dump a raw git object: blob bytes, tag fields, or a tree listing.\n\n\
        Blob content (`HEAD:path/to/file`, `:staged`, or a blob SHA) is \
        written byte-for-byte, so binary files round-trip cleanly and \
        `--limit` does not apply to it. Tags print in `git cat-file -p` \
        shape and trees as `<mode> <type> <sha> TAB <name>` lines. \
        For commits use `sak git show`.",
    after_help = "\
Examples:
  sak git cat-file HEAD:README.md       File content at HEAD
  sak git cat-file :staged-file.txt     Staged version of a file
  sak git cat-file v1.0                 Tag object fields
  sak git cat-file 'HEAD^{tree}'        Tree listing at HEAD"
)]
pub struct CatFileArgs {
    /// Path to the git repository
    #[arg(short = 'C', long)]
    pub repo: Option<PathBuf>,

    /// Revision resolving to a blob, tag, or tree (e.g. HEAD:file, v1.0)
    pub reference: String,

    /// Maximum output lines (line-mode outputs only; blob bytes are exact)
    #[arg(long)]
    pub limit: Option<usize>,
}

pub fn run(args: &CatFileArgs) -> Result<Outcome> {
    let repo = super::open_repo(&args.repo)?;

    let obj = match repo.revparse_single(&args.reference) {
        Ok(obj) => obj,
        Err(e) if e.code() == git2::ErrorCode::NotFound => return Ok(Outcome::NotFound),
        Err(e) => {
            return Err(e).with_context(|| format!("cannot resolve '{}'", args.reference));
        }
    };

    match obj.kind() {
        Some(git2::ObjectType::Blob) => dump_blob(&repo, &obj),
        Some(git2::ObjectType::Tag) => dump_tag(&repo, &obj, args.limit),
        Some(git2::ObjectType::Tree) => dump_tree(&repo, &obj, args.limit),
        Some(git2::ObjectType::Commit) => {
            bail!(
                "'{}' is a commit; use `sak git show {}` for commits",
                args.reference,
                args.reference
            )
        }
        _ => bail!("'{}' is not a blob, tag, or tree", args.reference),
    }
}

/// Write blob bytes verbatim (no line splitting, no limit): the dump is the
/// object, exactly. Mirrors the byte path in `fs head --bytes`.
fn dump_blob(repo: &git2::Repository, obj: &git2::Object) -> Result<Outcome> {
    let blob = repo.find_blob(obj.id())?;
    let content = blob.content();
    if content.is_empty() {
        return Ok(Outcome::NotFound);
    }
    let stdout = io::stdout();
    let mut handle = stdout.lock();
    handle.write_all(content)?;
    handle.flush()?;
    Ok(Outcome::Found)
}

/// Print tag fields in `git cat-file -p` shape.
fn dump_tag(repo: &git2::Repository, obj: &git2::Object, limit: Option<usize>) -> Result<Outcome> {
    let tag = repo.find_tag(obj.id())?;
    let target = tag.target()?;

    let stdout = io::stdout();
    let handle = stdout.lock();
    let mut writer = BoundedWriter::new(handle, limit);

    writer.write_line(&format!("object {}", target.id()))?;
    writer.write_line(&format!("type {}", object_type_name(tag.target_type())))?;
    writer.write_line(&format!("tag {}", tag.name().unwrap_or("")))?;
    if let Some(tagger) = tag.tagger() {
        let when = tagger.when();
        let offset = when.offset_minutes();
        let (sign, abs) = if offset < 0 {
            ('-', -offset)
        } else {
            ('+', offset)
        };
        writer.write_line(&format!(
            "tagger {} <{}> {} {}{:02}{:02}",
            tagger.name().unwrap_or(""),
            tagger.email().unwrap_or(""),
            when.seconds(),
            sign,
            abs / 60,
            abs % 60
        ))?;
    }
    writer.write_decoration("")?;
    if let Some(message) = tag.message() {
        for line in message.lines() {
            if !writer.write_line(line)? {
                break;
            }
        }
    }
    writer.flush()?;
    Ok(Outcome::Found)
}

/// Print one `<mode> <type> <sha> TAB <name>` line per tree entry, sorted by
/// name (git orders by raw bytes; name sort is friendlier and deterministic).
fn dump_tree(repo: &git2::Repository, obj: &git2::Object, limit: Option<usize>) -> Result<Outcome> {
    let tree = repo.find_tree(obj.id())?;
    if tree.is_empty() {
        return Ok(Outcome::NotFound);
    }

    let mut entries: Vec<(String, String, String, String)> = Vec::new();
    for entry in tree.iter() {
        entries.push((
            entry.name().unwrap_or("").to_string(),
            format!("{:06o}", entry.filemode()),
            object_type_name(entry.kind()).to_string(),
            entry.id().to_string(),
        ));
    }
    entries.sort_by(|a, b| a.0.cmp(&b.0));

    let stdout = io::stdout();
    let handle = stdout.lock();
    let mut writer = BoundedWriter::new(handle, limit);

    for (name, mode, kind, sha) in &entries {
        if !writer.write_line(&format!("{mode} {kind} {sha}\t{name}"))? {
            break;
        }
    }
    writer.flush()?;
    Ok(Outcome::Found)
}

fn object_type_name(kind: Option<git2::ObjectType>) -> &'static str {
    match kind {
        Some(git2::ObjectType::Commit) => "commit",
        Some(git2::ObjectType::Tree) => "tree",
        Some(git2::ObjectType::Blob) => "blob",
        Some(git2::ObjectType::Tag) => "tag",
        _ => "unknown",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cat_file_blob() {
        let (_dir, repo) = crate::git::init_test_repo();
        crate::git::create_test_commit(&repo, "blob content");

        let args = CatFileArgs {
            repo: Some(repo.workdir().unwrap().to_path_buf()),
            reference: "HEAD:blob_content.txt".to_string(),
            limit: None,
        };
        let result = run(&args).unwrap();
        assert_eq!(result, Outcome::Found);
    }

    #[test]
    fn test_cat_file_tree() {
        let (_dir, repo) = crate::git::init_test_repo();
        crate::git::create_test_commit(&repo, "tree content");

        let args = CatFileArgs {
            repo: Some(repo.workdir().unwrap().to_path_buf()),
            reference: "HEAD^{tree}".to_string(),
            limit: None,
        };
        let result = run(&args).unwrap();
        assert_eq!(result, Outcome::Found);
    }

    #[test]
    fn test_cat_file_commit_is_error() {
        let (_dir, repo) = crate::git::init_test_repo();
        crate::git::create_test_commit(&repo, "commit content");

        let args = CatFileArgs {
            repo: Some(repo.workdir().unwrap().to_path_buf()),
            reference: "HEAD".to_string(),
            limit: None,
        };
        let result = run(&args);
        assert!(result.is_err());
    }

    #[test]
    fn test_cat_file_bad_ref_is_not_found() {
        let (_dir, repo) = crate::git::init_test_repo();

        let args = CatFileArgs {
            repo: Some(repo.workdir().unwrap().to_path_buf()),
            reference: "deadbeef".to_string(),
            limit: None,
        };
        let result = run(&args).unwrap();
        assert_eq!(result, Outcome::NotFound);
    }
}
