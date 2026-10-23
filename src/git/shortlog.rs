use crate::output::Outcome;
use std::collections::HashMap;
use std::io;
use std::path::PathBuf;

use anyhow::Result;
use clap::Args;

use crate::output::BoundedWriter;

#[derive(Args)]
#[command(
    about = "Summarize commits by author",
    long_about = "Summarize history by author: one section per author with \
        their commit subjects.\n\n\
        Complements `sak git contributors` (flat author counts): shortlog \
        keeps the per-author commit titles, newest first within each section.",
    after_help = "\
Examples:
  sak git shortlog                  All authors with their subjects
  sak git shortlog -c 5             Top 5 authors only"
)]
pub struct ShortlogArgs {
    /// Path to the git repository
    #[arg(short = 'C', long)]
    pub repo: Option<PathBuf>,

    /// Maximum number of authors to show
    #[arg(short, long)]
    pub count: Option<usize>,

    /// Maximum output lines (author headers don't count)
    #[arg(long)]
    pub limit: Option<usize>,
}

pub fn run(args: &ShortlogArgs) -> Result<Outcome> {
    let repo = super::open_repo(&args.repo)?;

    let mut walker = repo.revwalk()?;
    if walker.push_head().is_err() {
        return Ok(Outcome::NotFound);
    }

    // Group commit subjects by author, preserving walk (newest-first) order
    // within each author.
    let mut by_author: HashMap<(String, String), Vec<String>> = HashMap::new();
    for oid in walker {
        let oid = oid?;
        let commit = repo.find_commit(oid)?;
        let author = commit.author();
        let key = (
            author.name().unwrap_or("").to_string(),
            author.email().unwrap_or("").to_string(),
        );
        let subject = commit.summary().unwrap_or("").to_string();
        by_author.entry(key).or_default().push(subject);
    }
    if by_author.is_empty() {
        return Ok(Outcome::NotFound);
    }

    let mut authors: Vec<((String, String), Vec<String>)> = by_author.into_iter().collect();
    authors.sort_by(|a, b| {
        b.1.len()
            .cmp(&a.1.len())
            .then(a.0.0.cmp(&b.0.0))
            .then(a.0.1.cmp(&b.0.1))
    });
    if let Some(n) = args.count {
        authors.truncate(n);
    }

    let stdout = io::stdout();
    let handle = stdout.lock();
    let mut writer = BoundedWriter::new(handle, args.limit);

    for ((name, email), subjects) in &authors {
        writer.write_decoration(&format!("{name} <{email}> ({}):", subjects.len()))?;
        for subject in subjects {
            if !writer.write_line(&format!("  {subject}"))? {
                break;
            }
        }
    }
    writer.flush()?;
    Ok(Outcome::Found)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_shortlog_groups_by_author() {
        let (_dir, repo) = crate::git::init_test_repo();
        crate::git::create_test_commit(&repo, "alpha change");
        crate::git::create_test_commit(&repo, "beta change");

        let args = ShortlogArgs {
            repo: Some(repo.workdir().unwrap().to_path_buf()),
            count: None,
            limit: None,
        };
        let result = run(&args).unwrap();
        assert_eq!(result, Outcome::Found);
    }

    #[test]
    fn test_shortlog_empty_repo_is_not_found() {
        let (_dir, repo) = crate::git::init_test_repo();

        let args = ShortlogArgs {
            repo: Some(repo.workdir().unwrap().to_path_buf()),
            count: None,
            limit: None,
        };
        let result = run(&args).unwrap();
        assert_eq!(result, Outcome::NotFound);
    }
}
