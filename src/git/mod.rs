pub mod blame;
pub mod branch;
pub mod contributors;
pub mod diff;
pub mod log;
pub mod remote;
pub mod show;
pub mod stash;
pub mod status;
pub mod tags;

use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{Context, Result};
use clap::Subcommand;
use git2::Repository;

#[derive(Subcommand)]
pub enum GitCommand {
    Status(status::StatusArgs),
    Diff(diff::DiffArgs),
    Log(log::LogArgs),
    Show(show::ShowArgs),
    Blame(blame::BlameArgs),
    Branch(branch::BranchArgs),
    Tags(tags::TagsArgs),
    Remote(remote::RemoteArgs),
    #[command(name = "stash-list")]
    StashList(stash::StashArgs),
    Contributors(contributors::ContributorsArgs),
}

pub fn run(cmd: &GitCommand) -> Result<ExitCode> {
    match cmd {
        GitCommand::Status(args) => status::run(args),
        GitCommand::Diff(args) => diff::run(args),
        GitCommand::Log(args) => log::run(args),
        GitCommand::Show(args) => show::run(args),
        GitCommand::Blame(args) => blame::run(args),
        GitCommand::Branch(args) => branch::run(args),
        GitCommand::Tags(args) => tags::run(args),
        GitCommand::Remote(args) => remote::run(args),
        GitCommand::StashList(args) => stash::run(args),
        GitCommand::Contributors(args) => contributors::run(args),
    }
}

/// Open a git repository, discovering it from the given path or cwd.
pub fn open_repo(path: &Option<PathBuf>) -> Result<Repository> {
    let start = match path {
        Some(p) => p.clone(),
        None => std::env::current_dir().context("cannot determine current directory")?,
    };
    Repository::discover(&start)
        .with_context(|| format!("not a git repository: {}", start.display()))
}

/// Format an OID as a short (7-char) hex string.
pub fn short_id(oid: git2::Oid) -> String {
    oid.to_string()[..7].to_string()
}

/// Format a git2::Time as an ISO 8601 date string.
pub fn format_time(time: git2::Time) -> String {
    let secs = time.seconds();
    let offset_minutes = time.offset_minutes();
    let offset_hours = offset_minutes / 60;
    let offset_mins = offset_minutes.unsigned_abs() % 60;
    let sign = if offset_minutes >= 0 { '+' } else { '-' };

    // Convert epoch seconds to date/time components
    let total_secs = secs + (offset_minutes as i64) * 60;
    let days = total_secs / 86400;
    let time_of_day = ((total_secs % 86400) + 86400) % 86400;
    let hours = time_of_day / 3600;
    let minutes = (time_of_day % 3600) / 60;
    let seconds = time_of_day % 60;

    // Days since epoch to Y-M-D (algorithm from http://howardhinnant.github.io/date_algorithms.html)
    let z = days + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };

    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}{}{:02}:{:02}",
        y,
        m,
        d,
        hours,
        minutes,
        seconds,
        sign,
        offset_hours.unsigned_abs(),
        offset_mins,
    )
}

/// Format a git2::Time as a short date string (YYYY-MM-DD).
pub fn format_date(time: git2::Time) -> String {
    format_time(time)[..10].to_string()
}

#[cfg(test)]
pub fn init_test_repo() -> (tempfile::TempDir, Repository) {
    let dir = tempfile::tempdir().expect("create temp dir");
    let repo = Repository::init(dir.path()).expect("init repo");
    {
        let mut config = repo.config().expect("get config");
        config.set_str("user.name", "Test User").expect("set name");
        config
            .set_str("user.email", "test@example.com")
            .expect("set email");
    }
    (dir, repo)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_open_repo_discovers_current() {
        // We're running inside the sak repo, so this should work
        let repo = open_repo(&None);
        assert!(repo.is_ok());
    }

    #[test]
    fn test_open_repo_not_a_repo() {
        let dir = tempfile::tempdir().unwrap();
        let result = open_repo(&Some(dir.path().to_path_buf()));
        assert!(result.is_err());
    }

    #[test]
    fn test_short_id() {
        let oid = git2::Oid::from_str("abc1234567890def1234567890abcdef12345678").unwrap();
        assert_eq!(short_id(oid), "abc1234");
    }

    #[test]
    fn test_format_time() {
        let time = git2::Time::new(1705312200, 0); // 2024-01-15T10:30:00+00:00
        let formatted = format_time(time);
        assert!(formatted.starts_with("2024-01-15T"));
        assert!(formatted.ends_with("+00:00"));
    }

    #[test]
    fn test_format_date() {
        let time = git2::Time::new(1705312200, 0);
        let date = format_date(time);
        assert_eq!(date, "2024-01-15");
    }

    #[test]
    fn test_init_test_repo() {
        let (_dir, repo) = init_test_repo();
        assert!(!repo.is_bare());
    }
}
