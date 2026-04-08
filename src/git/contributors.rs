use std::collections::HashMap;
use std::io;
use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::Result;
use clap::{Args, ValueEnum};
use git2::Sort;

use crate::output::BoundedWriter;

#[derive(Clone, ValueEnum)]
pub enum ContributorSort {
    Commits,
    Name,
}

#[derive(Args)]
#[command(
    about = "List contributors by commit count",
    long_about = "List contributors by commit count, aggregated from the commit history.\n\n\
        Output is tab-separated: count, name <email>.",
    after_help = "\
Examples:
  sak git contributors              All contributors by commit count
  sak git contributors -n 10        Top 10 contributors
  sak git contributors --sort name  Sort alphabetically"
)]
pub struct ContributorsArgs {
    /// Path to the git repository
    #[arg(short = 'C', long)]
    pub repo: Option<PathBuf>,

    /// Maximum number of contributors to show
    #[arg(short = 'n', long)]
    pub count: Option<usize>,

    /// Sort order
    #[arg(long, default_value = "commits")]
    pub sort: ContributorSort,

    /// Only count commits after this date (ISO 8601 or git date format)
    #[arg(long)]
    pub since: Option<String>,

    /// Only count commits before this date (ISO 8601 or git date format)
    #[arg(long)]
    pub until: Option<String>,

    /// Maximum output lines
    #[arg(long)]
    pub limit: Option<usize>,
}

pub fn run(args: &ContributorsArgs) -> Result<ExitCode> {
    let repo = super::open_repo(&args.repo)?;

    let mut revwalk = repo.revwalk()?;
    if revwalk.push_head().is_err() {
        return Ok(ExitCode::from(1)); // No HEAD (empty repo)
    }
    revwalk.set_sorting(Sort::TIME)?;

    let since_epoch = parse_date_to_epoch(&args.since)?;
    let until_epoch = parse_date_to_epoch(&args.until)?;

    let mut counts: HashMap<(String, String), usize> = HashMap::new();

    for oid in revwalk {
        let oid = oid?;
        let commit = repo.find_commit(oid)?;
        let time_secs = commit.time().seconds();

        if let Some(since) = since_epoch
            && time_secs < since
        {
            break; // Commits are in time order, so we can stop
        }
        if let Some(until) = until_epoch
            && time_secs > until
        {
            continue;
        }

        let author = commit.author();
        let name = author.name().unwrap_or("(unknown)").to_string();
        let email = author.email().unwrap_or("").to_string();
        *counts.entry((name, email)).or_insert(0) += 1;
    }

    if counts.is_empty() {
        return Ok(ExitCode::from(1));
    }

    let mut entries: Vec<((String, String), usize)> = counts.into_iter().collect();

    match args.sort {
        ContributorSort::Commits => entries.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.0.cmp(&b.0.0))),
        ContributorSort::Name => entries.sort_by(|a, b| a.0.0.cmp(&b.0.0)),
    }

    if let Some(count) = args.count {
        entries.truncate(count);
    }

    let max_count = entries.iter().map(|e| e.1).max().unwrap_or(0);
    let count_width = if max_count == 0 {
        1
    } else {
        ((max_count as f64).log10().floor() as usize) + 1
    };

    let stdout = io::stdout();
    let handle = stdout.lock();
    let mut writer = BoundedWriter::new(handle, args.limit);

    for ((name, email), count) in &entries {
        let line = if email.is_empty() {
            format!("{:>width$}\t{}", count, name, width = count_width)
        } else {
            format!(
                "{:>width$}\t{} <{}>",
                count,
                name,
                email,
                width = count_width
            )
        };
        if !writer.write_line(&line)? {
            break;
        }
    }
    writer.flush()?;

    Ok(ExitCode::SUCCESS)
}

pub fn parse_date_to_epoch(date: &Option<String>) -> Result<Option<i64>> {
    let Some(date_str) = date else {
        return Ok(None);
    };
    // Support YYYY-MM-DD format
    if let Some(epoch) = parse_iso_date(date_str) {
        return Ok(Some(epoch));
    }
    anyhow::bail!("unsupported date format: '{}' (use YYYY-MM-DD)", date_str)
}

fn parse_iso_date(s: &str) -> Option<i64> {
    let parts: Vec<&str> = s.split('-').collect();
    if parts.len() != 3 {
        return None;
    }
    let year: i64 = parts[0].parse().ok()?;
    let month: i64 = parts[1].parse().ok()?;
    let day: i64 = parts[2].parse().ok()?;
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    // Simple epoch calculation (not accounting for leap seconds, etc.)
    // Good enough for date filtering
    let mut days: i64 = 0;
    for y in 1970..year {
        days += if is_leap_year(y) { 366 } else { 365 };
    }
    let month_days = [
        31,
        28 + i64::from(is_leap_year(year)),
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    for d in month_days.iter().take((month - 1) as usize) {
        days += d;
    }
    days += day - 1;
    Some(days * 86400)
}

fn is_leap_year(y: i64) -> bool {
    (y % 4 == 0 && y % 100 != 0) || y % 400 == 0
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
    fn test_contributors_with_commits() {
        let (_dir, repo) = crate::git::init_test_repo();
        create_commit(&repo, "first");
        create_commit(&repo, "second");

        let args = ContributorsArgs {
            repo: Some(repo.workdir().unwrap().to_path_buf()),
            count: None,
            sort: ContributorSort::Commits,
            since: None,
            until: None,
            limit: None,
        };
        let result = run(&args).unwrap();
        assert_eq!(result, ExitCode::SUCCESS);
    }

    #[test]
    fn test_contributors_empty_repo() {
        let (_dir, repo) = crate::git::init_test_repo();
        let args = ContributorsArgs {
            repo: Some(repo.workdir().unwrap().to_path_buf()),
            count: None,
            sort: ContributorSort::Commits,
            since: None,
            until: None,
            limit: None,
        };
        let result = run(&args).unwrap();
        assert_eq!(result, ExitCode::from(1));
    }

    #[test]
    fn test_parse_iso_date() {
        let epoch = parse_iso_date("2024-01-01").unwrap();
        assert!(epoch > 0);
        assert!(parse_iso_date("not-a-date").is_none());
    }
}
