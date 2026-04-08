use std::io;
use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{Context, Result};
use clap::Args;
use git2::BlameOptions;

use crate::output::{BoundedWriter, format_line_number, line_number_width};

#[derive(Args)]
#[command(
    about = "Show line-by-line authorship",
    long_about = "Show line-by-line authorship for a file.\n\n\
        Each line shows the commit, author, date, and content.",
    after_help = "\
Examples:
  sak git blame src/main.rs             Blame entire file
  sak git blame -L 10,20 src/main.rs    Blame lines 10-20
  sak git blame --limit 50 src/main.rs  First 50 lines"
)]
pub struct BlameArgs {
    /// Path to the git repository
    #[arg(short = 'C', long)]
    pub repo: Option<PathBuf>,

    /// File to blame
    pub file: PathBuf,

    /// Line range (e.g., "10,20" or "10,+5")
    #[arg(short = 'L', long)]
    pub lines: Option<String>,

    /// Maximum output lines
    #[arg(long)]
    pub limit: Option<usize>,
}

pub fn run(args: &BlameArgs) -> Result<ExitCode> {
    let repo = super::open_repo(&args.repo)?;

    let mut blame_opts = BlameOptions::new();

    if let Some(ref range) = args.lines {
        let (start, end) = parse_line_range(range)?;
        blame_opts.min_line(start);
        blame_opts.max_line(end);
    }

    // Resolve file path relative to repo workdir
    let workdir = repo
        .workdir()
        .context("bare repositories are not supported")?;
    let file_path = if args.file.is_absolute() {
        args.file
            .strip_prefix(workdir)
            .with_context(|| {
                format!(
                    "file '{}' is not inside the repository",
                    args.file.display()
                )
            })?
            .to_path_buf()
    } else {
        args.file.clone()
    };

    let blame = repo
        .blame_file(&file_path, Some(&mut blame_opts))
        .with_context(|| format!("cannot blame: {}", file_path.display()))?;

    // Read file content for line text
    let full_path = workdir.join(&file_path);
    let content = std::fs::read_to_string(&full_path)
        .with_context(|| format!("cannot read: {}", full_path.display()))?;
    let lines: Vec<&str> = content.lines().collect();

    let (start_line, end_line) = if let Some(ref range) = args.lines {
        let (s, e) = parse_line_range(range)?;
        (s, e.min(lines.len()))
    } else {
        (1, lines.len())
    };

    if start_line > lines.len() {
        return Ok(ExitCode::from(1));
    }

    let width = line_number_width(end_line);

    let stdout = io::stdout();
    let handle = stdout.lock();
    let mut writer = BoundedWriter::new(handle, args.limit);

    for line_num in start_line..=end_line {
        let line_content = lines.get(line_num - 1).unwrap_or(&"");

        if let Some(hunk) = blame.get_line(line_num) {
            let oid = hunk.final_commit_id();
            let sig = hunk.final_signature();
            let author = sig.name().unwrap_or("(unknown)");

            let date = if let Ok(commit) = repo.find_commit(oid) {
                super::format_date(commit.time())
            } else {
                "0000-00-00".to_string()
            };

            let output = format!(
                "{}  {}  {}  {}{}",
                super::short_id(oid),
                author,
                date,
                format_line_number(line_num, width),
                line_content,
            );
            if !writer.write_line(&output)? {
                break;
            }
        }
    }

    writer.flush()?;
    Ok(ExitCode::SUCCESS)
}

fn parse_line_range(range: &str) -> Result<(usize, usize)> {
    if let Some((start_str, rest)) = range.split_once(',') {
        let start: usize = start_str.parse().context("invalid start line in range")?;
        if let Some(offset_str) = rest.strip_prefix('+') {
            let offset: usize = offset_str.parse().context("invalid offset in range")?;
            Ok((start, start + offset - 1))
        } else {
            let end: usize = rest.parse().context("invalid end line in range")?;
            Ok((start, end))
        }
    } else {
        let line: usize = range.parse().context("invalid line number")?;
        Ok((line, line))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn create_committed_file(repo: &git2::Repository, filename: &str, content: &str) {
        let dir = repo.workdir().unwrap();
        fs::write(dir.join(filename), content).unwrap();
        let mut index = repo.index().unwrap();
        index.add_path(std::path::Path::new(filename)).unwrap();
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
        repo.commit(
            Some("HEAD"),
            &sig,
            &sig,
            &format!("add {}", filename),
            &tree,
            &parent_refs,
        )
        .unwrap();
    }

    #[test]
    fn test_blame_file() {
        let (_dir, repo) = crate::git::init_test_repo();
        create_committed_file(&repo, "test.txt", "line1\nline2\nline3\n");

        let args = BlameArgs {
            repo: Some(repo.workdir().unwrap().to_path_buf()),
            file: PathBuf::from("test.txt"),
            lines: None,
            limit: None,
        };
        let result = run(&args).unwrap();
        assert_eq!(result, ExitCode::SUCCESS);
    }

    #[test]
    fn test_blame_line_range() {
        let (_dir, repo) = crate::git::init_test_repo();
        create_committed_file(&repo, "test.txt", "line1\nline2\nline3\nline4\nline5\n");

        let args = BlameArgs {
            repo: Some(repo.workdir().unwrap().to_path_buf()),
            file: PathBuf::from("test.txt"),
            lines: Some("2,4".to_string()),
            limit: None,
        };
        let result = run(&args).unwrap();
        assert_eq!(result, ExitCode::SUCCESS);
    }

    #[test]
    fn test_parse_line_range_simple() {
        let (start, end) = parse_line_range("10,20").unwrap();
        assert_eq!(start, 10);
        assert_eq!(end, 20);
    }

    #[test]
    fn test_parse_line_range_offset() {
        let (start, end) = parse_line_range("10,+5").unwrap();
        assert_eq!(start, 10);
        assert_eq!(end, 14);
    }

    #[test]
    fn test_parse_line_range_single() {
        let (start, end) = parse_line_range("42").unwrap();
        assert_eq!(start, 42);
        assert_eq!(end, 42);
    }
}
