use std::io;
use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::Result;
use clap::Args;
use git2::Sort;

use crate::output::BoundedWriter;

use super::contributors::parse_date_to_epoch;

#[derive(Args)]
#[command(
    about = "View commit history",
    long_about = "View commit history with flexible filtering.\n\n\
        Supports filtering by date range, author, message pattern, and paths.",
    after_help = "\
Examples:
  sak git log                        Full log
  sak git log --oneline -n 10        Last 10 commits, compact
  sak git log --author alice         Commits by alice
  sak git log --since 2024-01-01     Commits since date
  sak git log -- src/                Commits touching src/"
)]
pub struct LogArgs {
    /// Path to the git repository
    #[arg(short = 'C', long)]
    pub repo: Option<PathBuf>,

    /// Show compact one-line format
    #[arg(long)]
    pub oneline: bool,

    /// Maximum number of commits to show
    #[arg(short = 'n', long)]
    pub count: Option<usize>,

    /// Show commits after this date (YYYY-MM-DD)
    #[arg(long)]
    pub since: Option<String>,

    /// Show commits before this date (YYYY-MM-DD)
    #[arg(long)]
    pub until: Option<String>,

    /// Filter by author name or email (substring match)
    #[arg(long)]
    pub author: Option<String>,

    /// Filter by commit message (substring match)
    #[arg(long)]
    pub grep: Option<String>,

    /// Restrict to commits touching these paths
    #[arg(last = true)]
    pub paths: Vec<PathBuf>,

    /// Maximum output lines
    #[arg(long)]
    pub limit: Option<usize>,
}

pub fn run(args: &LogArgs) -> Result<ExitCode> {
    let repo = super::open_repo(&args.repo)?;

    let mut revwalk = repo.revwalk()?;
    if revwalk.push_head().is_err() {
        return Ok(ExitCode::from(1)); // No HEAD (empty repo)
    }
    revwalk.set_sorting(Sort::TIME)?;

    let since_epoch = parse_date_to_epoch(&args.since)?;
    let until_epoch = parse_date_to_epoch(&args.until)?;

    let stdout = io::stdout();
    let handle = stdout.lock();
    let mut writer = BoundedWriter::new(handle, args.limit);

    let mut shown = 0usize;
    let mut first = true;

    for oid in revwalk {
        let oid = oid?;
        let commit = repo.find_commit(oid)?;
        let time_secs = commit.time().seconds();

        // Date filters
        if let Some(since) = since_epoch
            && time_secs < since
        {
            break;
        }
        if let Some(until) = until_epoch
            && time_secs > until
        {
            continue;
        }

        // Author filter
        if let Some(ref author_filter) = args.author {
            let author = commit.author();
            let name = author.name().unwrap_or("");
            let email = author.email().unwrap_or("");
            if !name.contains(author_filter.as_str()) && !email.contains(author_filter.as_str()) {
                continue;
            }
        }

        // Message filter
        if let Some(ref grep) = args.grep {
            let message = commit.message().unwrap_or("");
            if !message.contains(grep.as_str()) {
                continue;
            }
        }

        // Path filter
        if !args.paths.is_empty() && !commit_touches_paths(&repo, &commit, &args.paths)? {
            continue;
        }

        // Count limit
        if let Some(count) = args.count
            && shown >= count
        {
            break;
        }

        if args.oneline {
            let summary = commit.summary().unwrap_or("");
            let line = format!("{} {}", super::short_id(oid), summary);
            if !writer.write_line(&line)? {
                break;
            }
        } else {
            if !first {
                writer.write_decoration("")?;
            }
            first = false;

            let author = commit.author();
            let name = author.name().unwrap_or("(unknown)");
            let email = author.email().unwrap_or("");
            let date = super::format_time(commit.time());

            writer.write_decoration(&format!("commit {}", oid))?;
            writer.write_decoration(&format!("Author: {} <{}>", name, email))?;
            writer.write_decoration(&format!("Date:   {}", date))?;
            writer.write_decoration("")?;

            let message = commit.message().unwrap_or("");
            for line in message.lines() {
                if !writer.write_line(&format!("    {}", line))? {
                    break;
                }
            }
        }

        shown += 1;
    }

    if shown == 0 {
        return Ok(ExitCode::from(1));
    }

    writer.flush()?;
    Ok(ExitCode::SUCCESS)
}

fn commit_touches_paths(
    repo: &git2::Repository,
    commit: &git2::Commit,
    paths: &[PathBuf],
) -> Result<bool> {
    let commit_tree = commit.tree()?;

    let parent_tree = if commit.parent_count() > 0 {
        Some(commit.parent(0)?.tree()?)
    } else {
        None
    };

    let diff = repo.diff_tree_to_tree(parent_tree.as_ref(), Some(&commit_tree), None)?;

    for delta in diff.deltas() {
        let old_path = delta.old_file().path();
        let new_path = delta.new_file().path();
        for filter_path in paths {
            if let Some(old) = old_path
                && old.starts_with(filter_path)
            {
                return Ok(true);
            }
            if let Some(new) = new_path
                && new.starts_with(filter_path)
            {
                return Ok(true);
            }
        }
    }

    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn create_commit(repo: &git2::Repository, message: &str) {
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
            .unwrap();
    }

    #[test]
    fn test_log_with_commits() {
        let (_dir, repo) = crate::git::init_test_repo();
        create_commit(&repo, "first commit");
        create_commit(&repo, "second commit");

        let args = LogArgs {
            repo: Some(repo.workdir().unwrap().to_path_buf()),
            oneline: true,
            count: None,
            since: None,
            until: None,
            author: None,
            grep: None,
            paths: vec![],
            limit: None,
        };
        let result = run(&args).unwrap();
        assert_eq!(result, ExitCode::SUCCESS);
    }

    #[test]
    fn test_log_count() {
        let (_dir, repo) = crate::git::init_test_repo();
        create_commit(&repo, "first");
        create_commit(&repo, "second");
        create_commit(&repo, "third");

        let args = LogArgs {
            repo: Some(repo.workdir().unwrap().to_path_buf()),
            oneline: true,
            count: Some(2),
            since: None,
            until: None,
            author: None,
            grep: None,
            paths: vec![],
            limit: None,
        };
        let result = run(&args).unwrap();
        assert_eq!(result, ExitCode::SUCCESS);
    }

    #[test]
    fn test_log_empty_repo() {
        let (_dir, repo) = crate::git::init_test_repo();
        let args = LogArgs {
            repo: Some(repo.workdir().unwrap().to_path_buf()),
            oneline: false,
            count: None,
            since: None,
            until: None,
            author: None,
            grep: None,
            paths: vec![],
            limit: None,
        };
        let result = run(&args).unwrap();
        assert_eq!(result, ExitCode::from(1));
    }
}
