use std::io;
use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::Result;
use clap::Args;

use crate::output::BoundedWriter;

#[derive(Args)]
#[command(
    about = "List remotes",
    long_about = "List remote repositories and their URLs.\n\n\
        Output is tab-separated: name, URL.",
    after_help = "\
Examples:
  sak git remote                    List all remotes
  sak git remote -C /path/to/repo   Remotes for another repo"
)]
pub struct RemoteArgs {
    /// Path to the git repository
    #[arg(short = 'C', long)]
    pub repo: Option<PathBuf>,

    /// Maximum output lines
    #[arg(long)]
    pub limit: Option<usize>,
}

pub fn run(args: &RemoteArgs) -> Result<ExitCode> {
    let repo = super::open_repo(&args.repo)?;

    let remotes = repo.remotes()?;

    let mut entries: Vec<(String, String)> = Vec::new();
    for name in remotes.iter().flatten() {
        let remote = repo.find_remote(name)?;
        let url = remote.url().unwrap_or("(no url)").to_string();
        entries.push((name.to_string(), url));
    }

    if entries.is_empty() {
        return Ok(ExitCode::from(1));
    }

    entries.sort_by(|a, b| a.0.cmp(&b.0));

    let stdout = io::stdout();
    let handle = stdout.lock();
    let mut writer = BoundedWriter::new(handle, args.limit);

    for (name, url) in &entries {
        if !writer.write_line(&format!("{}\t{}", name, url))? {
            break;
        }
    }
    writer.flush()?;

    Ok(ExitCode::SUCCESS)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_remote_empty() {
        let (_dir, repo) = crate::git::init_test_repo();
        let args = RemoteArgs {
            repo: Some(repo.workdir().unwrap().to_path_buf()),
            limit: None,
        };
        let result = run(&args).unwrap();
        assert_eq!(result, ExitCode::from(1));
    }

    #[test]
    fn test_remote_with_remote() {
        let (_dir, repo) = crate::git::init_test_repo();
        repo.remote("origin", "https://example.com/repo.git")
            .unwrap();

        let args = RemoteArgs {
            repo: Some(repo.workdir().unwrap().to_path_buf()),
            limit: None,
        };
        let result = run(&args).unwrap();
        assert_eq!(result, ExitCode::SUCCESS);
    }
}
