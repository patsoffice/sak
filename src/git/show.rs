use std::io;
use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{Context, Result};
use clap::Args;
use git2::DiffFormat;

use crate::output::BoundedWriter;

#[derive(Args)]
#[command(
    about = "Show a commit",
    long_about = "Display a specific commit's metadata and diff.\n\n\
        Shows commit info (author, date, message) followed by the diff.",
    after_help = "\
Examples:
  sak git show                       Show HEAD commit
  sak git show HEAD~3                Show 3 commits ago
  sak git show --stat                Show with diffstat
  sak git show --name-only           Show changed files only
  sak git show --format '%h %s'      Custom format"
)]
pub struct ShowArgs {
    /// Path to the git repository
    #[arg(short = 'C', long)]
    pub repo: Option<PathBuf>,

    /// Commit, tag, or ref to show (default: HEAD)
    #[arg(default_value = "HEAD")]
    pub reference: String,

    /// Show diffstat summary
    #[arg(long)]
    pub stat: bool,

    /// Show only file names
    #[arg(long)]
    pub name_only: bool,

    /// Custom format string (%H hash, %h short hash, %an author, %ae email, %s summary, %b body)
    #[arg(long)]
    pub format: Option<String>,

    /// Maximum output lines
    #[arg(long)]
    pub limit: Option<usize>,
}

pub fn run(args: &ShowArgs) -> Result<ExitCode> {
    let repo = super::open_repo(&args.repo)?;

    let obj = repo
        .revparse_single(&args.reference)
        .with_context(|| format!("cannot resolve '{}'", args.reference))?;
    let commit = obj
        .peel_to_commit()
        .with_context(|| format!("'{}' is not a commit", args.reference))?;

    let stdout = io::stdout();
    let handle = stdout.lock();
    let mut writer = BoundedWriter::new(handle, args.limit);

    // Output commit metadata
    if let Some(ref fmt) = args.format {
        let line = expand_format(fmt, &commit);
        writer.write_line(&line)?;
    } else {
        let author = commit.author();
        let name = author.name().unwrap_or("(unknown)");
        let email = author.email().unwrap_or("");
        let date = super::format_time(commit.time());

        writer.write_decoration(&format!("commit {}", commit.id()))?;
        writer.write_decoration(&format!("Author: {} <{}>", name, email))?;
        writer.write_decoration(&format!("Date:   {}", date))?;
        writer.write_decoration("")?;

        let message = commit.message().unwrap_or("");
        for line in message.lines() {
            writer.write_decoration(&format!("    {}", line))?;
        }
        writer.write_decoration("")?;
    }

    // Diff commit against parent
    let commit_tree = commit.tree()?;
    let parent_tree = if commit.parent_count() > 0 {
        Some(commit.parent(0)?.tree()?)
    } else {
        None
    };

    let diff = repo.diff_tree_to_tree(parent_tree.as_ref(), Some(&commit_tree), None)?;

    if args.name_only {
        let mut names: Vec<String> = Vec::new();
        for delta in diff.deltas() {
            let path = delta
                .new_file()
                .path()
                .or_else(|| delta.old_file().path())
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_default();
            names.push(path);
        }
        names.sort();
        names.dedup();
        for name in &names {
            if !writer.write_line(name)? {
                break;
            }
        }
    } else if args.stat {
        let stats = diff.stats()?;
        let buf = stats.to_buf(git2::DiffStatsFormat::FULL, 80)?;
        let text = buf.as_str().unwrap_or("");
        for line in text.lines() {
            if !writer.write_line(line)? {
                break;
            }
        }
    } else {
        diff.print(DiffFormat::Patch, |_delta, _hunk, line| {
            let prefix = match line.origin() {
                '+' => "+",
                '-' => "-",
                ' ' => " ",
                _ => "",
            };
            let content = std::str::from_utf8(line.content()).unwrap_or("");
            let text = format!("{}{}", prefix, content.trim_end_matches('\n'));
            let _ = writer.write_line(&text);
            true
        })?;
    }

    writer.flush()?;
    Ok(ExitCode::SUCCESS)
}

fn expand_format(fmt: &str, commit: &git2::Commit) -> String {
    let mut result = String::new();
    let mut chars = fmt.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '%' {
            match chars.peek() {
                Some('H') => {
                    chars.next();
                    result.push_str(&commit.id().to_string());
                }
                Some('h') => {
                    chars.next();
                    result.push_str(&super::short_id(commit.id()));
                }
                Some('a') => {
                    chars.next();
                    match chars.peek() {
                        Some('n') => {
                            chars.next();
                            result.push_str(commit.author().name().unwrap_or(""));
                        }
                        Some('e') => {
                            chars.next();
                            result.push_str(commit.author().email().unwrap_or(""));
                        }
                        _ => result.push_str("%a"),
                    }
                }
                Some('s') => {
                    chars.next();
                    result.push_str(commit.summary().unwrap_or(""));
                }
                Some('b') => {
                    chars.next();
                    result.push_str(commit.body().unwrap_or(""));
                }
                Some('%') => {
                    chars.next();
                    result.push('%');
                }
                _ => result.push('%'),
            }
        } else {
            result.push(ch);
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn create_commit(repo: &git2::Repository, message: &str) -> git2::Oid {
        let dir = repo.workdir().unwrap();
        let filename = format!("{}.txt", message.replace(' ', "_"));
        fs::write(dir.join(&filename), message).unwrap();
        let mut index = repo.index().unwrap();
        index.add_path(std::path::Path::new(&filename)).unwrap();
        index.write().unwrap();
        let tree_oid = index.write_tree().unwrap();
        let tree = repo.find_tree(tree_oid).unwrap();
        let sig = repo.signature().unwrap();
        let parents: Vec<git2::Commit> = if let Ok(head) = repo.head() {
            vec![head.peel_to_commit().unwrap()]
        } else {
            vec![]
        };
        let parent_refs: Vec<&git2::Commit> = parents.iter().collect();
        repo.commit(Some("HEAD"), &sig, &sig, message, &tree, &parent_refs)
            .unwrap()
    }

    #[test]
    fn test_show_head() {
        let (_dir, repo) = crate::git::init_test_repo();
        create_commit(&repo, "test commit");

        let args = ShowArgs {
            repo: Some(repo.workdir().unwrap().to_path_buf()),
            reference: "HEAD".to_string(),
            stat: false,
            name_only: false,
            format: None,
            limit: None,
        };
        let result = run(&args).unwrap();
        assert_eq!(result, ExitCode::SUCCESS);
    }

    #[test]
    fn test_show_name_only() {
        let (_dir, repo) = crate::git::init_test_repo();
        create_commit(&repo, "test commit");

        let args = ShowArgs {
            repo: Some(repo.workdir().unwrap().to_path_buf()),
            reference: "HEAD".to_string(),
            stat: false,
            name_only: true,
            format: None,
            limit: None,
        };
        let result = run(&args).unwrap();
        assert_eq!(result, ExitCode::SUCCESS);
    }

    #[test]
    fn test_expand_format() {
        let (_dir, repo) = crate::git::init_test_repo();
        let oid = create_commit(&repo, "test message");
        let commit = repo.find_commit(oid).unwrap();

        let result = expand_format("%h %s", &commit);
        assert!(result.contains("test message"));
        assert_eq!(result.split(' ').next().unwrap().len(), 7);
    }
}
