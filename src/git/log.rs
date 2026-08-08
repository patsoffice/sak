use crate::output::Outcome;
use std::io;
use std::path::PathBuf;

use anyhow::Result;
use clap::Args;
use git2::Sort;

use crate::output::BoundedWriter;

use super::contributors::parse_date_to_epoch;

#[derive(Args, Default)]
#[command(
    about = "View commit history",
    long_about = "View commit history with flexible filtering.\n\n\
        Supports filtering by date range, author, message pattern, and paths.\n\n\
        `-n N` caps the number of commits, and git's `-1`..`-9` shorthand is \
        accepted for the same thing (`sak git log -1 --stat` is the usual \
        \"what did that commit do\" idiom). `--stat` appends a per-commit \
        diffstat against the first parent, restricted to the given paths.",
    after_help = "\
Examples:
  sak git log                        Full log
  sak git log --oneline -n 10        Last 10 commits, compact
  sak git log -1 --stat              Latest commit plus its diffstat
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

    /// Show a diffstat for each commit (against its first parent)
    #[arg(long)]
    pub stat: bool,

    // git's `-<n>` count shorthand. clap has no pattern-matched shorts, so the
    // single-digit forms are spelled out as hidden flags — they cover `git log
    // -1`, which is the muscle memory worth honoring; larger counts use `-n N`,
    // which has no upper bound. `count_shorthand` folds them back into one
    // value, and `conflicts_with` keeps `-1 -n 5` from silently picking one.
    #[arg(short = '1', hide = true, conflicts_with = "count")]
    pub n1: bool,
    #[arg(short = '2', hide = true, conflicts_with = "count")]
    pub n2: bool,
    #[arg(short = '3', hide = true, conflicts_with = "count")]
    pub n3: bool,
    #[arg(short = '4', hide = true, conflicts_with = "count")]
    pub n4: bool,
    #[arg(short = '5', hide = true, conflicts_with = "count")]
    pub n5: bool,
    #[arg(short = '6', hide = true, conflicts_with = "count")]
    pub n6: bool,
    #[arg(short = '7', hide = true, conflicts_with = "count")]
    pub n7: bool,
    #[arg(short = '8', hide = true, conflicts_with = "count")]
    pub n8: bool,
    #[arg(short = '9', hide = true, conflicts_with = "count")]
    pub n9: bool,

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

impl LogArgs {
    /// The commit cap, from `-n N` or git's `-1`..`-9` shorthand.
    fn commit_count(&self) -> Option<usize> {
        self.count.or_else(|| {
            [
                self.n1, self.n2, self.n3, self.n4, self.n5, self.n6, self.n7, self.n8, self.n9,
            ]
            .iter()
            .position(|&set| set)
            .map(|i| i + 1)
        })
    }
}

pub fn run(args: &LogArgs) -> Result<Outcome> {
    let repo = super::open_repo(&args.repo)?;

    let mut revwalk = repo.revwalk()?;
    if revwalk.push_head().is_err() {
        return Ok(Outcome::NotFound); // No HEAD (empty repo)
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
        if let Some(count) = args.commit_count()
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

        if args.stat {
            let diff = parent_diff(&repo, &commit, &args.paths)?;
            let buf = diff.stats()?.to_buf(git2::DiffStatsFormat::FULL, 80)?;
            if !args.oneline {
                writer.write_decoration("")?;
            }
            for line in buf.as_str().unwrap_or("").lines() {
                if !writer.write_line(line)? {
                    break;
                }
            }
        }

        shown += 1;
    }

    if shown == 0 {
        return Ok(Outcome::NotFound);
    }

    writer.flush()?;
    Ok(Outcome::Found)
}

/// Diff a commit against its first parent (or against nothing, for a root
/// commit). `paths` becomes a pathspec so `--stat -- src/` reports only the
/// files the caller asked about, the way `git log --stat -- src/` does.
fn parent_diff<'a>(
    repo: &'a git2::Repository,
    commit: &git2::Commit,
    paths: &[PathBuf],
) -> Result<git2::Diff<'a>> {
    let commit_tree = commit.tree()?;
    let parent_tree = if commit.parent_count() > 0 {
        Some(commit.parent(0)?.tree()?)
    } else {
        None
    };

    let mut opts = git2::DiffOptions::new();
    for path in paths {
        opts.pathspec(path);
    }
    Ok(repo.diff_tree_to_tree(parent_tree.as_ref(), Some(&commit_tree), Some(&mut opts))?)
}

fn commit_touches_paths(
    repo: &git2::Repository,
    commit: &git2::Commit,
    paths: &[PathBuf],
) -> Result<bool> {
    // Deliberately *not* `parent_diff`'s pathspec: this is the commit-selection
    // filter, and it keeps its own component-wise prefix match so the set of
    // commits `log -- <path>` shows doesn't shift under this refactor.
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

    fn args_for(repo: &git2::Repository) -> LogArgs {
        LogArgs {
            repo: Some(repo.workdir().unwrap().to_path_buf()),
            oneline: true,
            ..Default::default()
        }
    }

    #[test]
    fn test_log_with_commits() {
        let (_dir, repo) = crate::git::init_test_repo();
        create_commit(&repo, "first commit");
        create_commit(&repo, "second commit");

        let result = run(&args_for(&repo)).unwrap();
        assert_eq!(result, Outcome::Found);
    }

    #[test]
    fn test_log_count() {
        let (_dir, repo) = crate::git::init_test_repo();
        create_commit(&repo, "first");
        create_commit(&repo, "second");
        create_commit(&repo, "third");

        let args = LogArgs {
            count: Some(2),
            ..args_for(&repo)
        };
        let result = run(&args).unwrap();
        assert_eq!(result, Outcome::Found);
    }

    #[test]
    fn test_log_empty_repo() {
        let (_dir, repo) = crate::git::init_test_repo();
        let args = LogArgs {
            oneline: false,
            ..args_for(&repo)
        };
        let result = run(&args).unwrap();
        assert_eq!(result, Outcome::NotFound);
    }

    #[test]
    fn digit_shorthand_resolves_to_a_count() {
        // `-n` wins outright; otherwise the lowest set digit is the count.
        // clap's conflicts_with keeps the two from ever both being set.
        let explicit = LogArgs {
            count: Some(7),
            n2: true,
            ..Default::default()
        };
        assert_eq!(explicit.commit_count(), Some(7));

        let shorthand = LogArgs {
            n3: true,
            ..Default::default()
        };
        assert_eq!(shorthand.commit_count(), Some(3));

        let ninth = LogArgs {
            n9: true,
            ..Default::default()
        };
        assert_eq!(ninth.commit_count(), Some(9));

        assert_eq!(LogArgs::default().commit_count(), None);
    }

    #[test]
    fn clap_accepts_git_style_dash_one() {
        use clap::Parser;

        #[derive(Parser)]
        struct LogCli {
            #[command(flatten)]
            args: LogArgs,
        }

        let cli = LogCli::try_parse_from(["log", "-1", "--stat"]).unwrap();
        assert_eq!(cli.args.commit_count(), Some(1));
        assert!(cli.args.stat);
        // `-1` and `-n` are mutually exclusive rather than silently one-wins.
        assert!(LogCli::try_parse_from(["log", "-1", "-n", "5"]).is_err());
    }

    #[test]
    fn stat_reports_the_files_a_commit_touched() {
        let (_dir, repo) = crate::git::init_test_repo();
        create_commit(&repo, "only commit");

        let args = LogArgs {
            stat: true,
            n1: true,
            ..args_for(&repo)
        };
        assert_eq!(run(&args).unwrap(), Outcome::Found);

        // The stat comes off the same first-parent diff the path filter uses,
        // so a root commit (no parent) still reports its files rather than
        // erroring on the missing parent.
        let head = repo.head().unwrap().peel_to_commit().unwrap();
        let root = {
            let mut c = head.clone();
            while c.parent_count() > 0 {
                c = c.parent(0).unwrap();
            }
            c
        };
        let diff = parent_diff(&repo, &root, &[]).unwrap();
        assert!(diff.deltas().len() > 0);
    }
}
