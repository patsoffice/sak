use std::io;
use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{Context, Result};
use clap::{Args, ValueEnum};
use globset::{Glob, GlobMatcher};
use walkdir::WalkDir;

use crate::output::{BoundedWriter, relative_path};

#[derive(Clone, ValueEnum)]
pub enum EntryType {
    File,
    Dir,
    Symlink,
}

#[derive(Clone, ValueEnum)]
pub enum SortOrder {
    Name,
    Modified,
    Size,
    None,
}

#[derive(Args)]
#[command(
    about = "Find files matching glob patterns",
    long_about = "Find files matching glob patterns.\n\n\
        Recursively searches directories for entries matching the given glob pattern. \
        Supports ** for recursive matching and {a,b} alternation.",
    after_help = "\
Examples:
  sak fs glob '**/*.rs'                    Find all Rust files
  sak fs glob '**/*.rs' src/               Find Rust files under src/
  sak fs glob 'src/{main,lib}.rs'          Find specific files
  sak fs glob '**/*' --type dir            List all directories
  sak fs glob '**/*.log' --max-depth 2     Shallow search for log files"
)]
pub struct GlobArgs {
    /// Glob pattern (e.g., "**/*.rs", "src/{main,lib}.rs")
    pub pattern: String,

    /// Root directory to search
    #[arg(default_value = ".")]
    pub path: PathBuf,

    /// Filter by entry type
    #[arg(short = 't', long = "type", default_value = "file")]
    pub entry_type: EntryType,

    /// Maximum directory depth to recurse
    #[arg(short = 'd', long)]
    pub max_depth: Option<usize>,

    /// Include hidden files and directories (dotfiles)
    #[arg(short = 'H', long)]
    pub hidden: bool,

    /// Follow symbolic links
    #[arg(short = 'L', long)]
    pub follow_links: bool,

    /// Sort order for results
    #[arg(long, default_value = "name")]
    pub sort: SortOrder,

    /// Maximum number of results to return
    #[arg(long)]
    pub limit: Option<usize>,
}

const SKIP_DIRS: &[&str] = &[".git", "target", "node_modules", "__pycache__", ".venv"];

fn should_skip(name: &str, hidden: bool) -> bool {
    if !hidden && name.starts_with('.') {
        return true;
    }
    SKIP_DIRS.contains(&name)
}

fn matches_type(entry: &walkdir::DirEntry, entry_type: &EntryType) -> bool {
    let ft = entry.file_type();
    match entry_type {
        EntryType::File => ft.is_file(),
        EntryType::Dir => ft.is_dir(),
        EntryType::Symlink => ft.is_symlink(),
    }
}

pub fn run(args: &GlobArgs) -> Result<ExitCode> {
    let glob = Glob::new(&args.pattern)
        .with_context(|| format!("invalid glob pattern: {}", args.pattern))?;
    let matcher: GlobMatcher = glob.compile_matcher();

    let base = args
        .path
        .canonicalize()
        .with_context(|| format!("cannot access directory: {}", args.path.display()))?;

    let mut walker = WalkDir::new(&base).follow_links(args.follow_links);
    if let Some(depth) = args.max_depth {
        walker = walker.max_depth(depth);
    }

    let mut results: Vec<(PathBuf, std::fs::Metadata)> = Vec::new();
    let hidden = args.hidden;

    let iter = walker.into_iter().filter_entry(move |e| {
        if e.depth() > 0
            && e.file_type().is_dir()
            && let Some(name) = e.file_name().to_str()
        {
            return !should_skip(name, hidden);
        }
        true
    });

    for entry in iter {
        let entry = match entry {
            Ok(e) => e,
            Err(e) => {
                eprintln!("sak: error: {}", e);
                continue;
            }
        };

        if !matches_type(&entry, &args.entry_type) {
            continue;
        }

        // Skip hidden files (not dirs — those are handled by filter_entry)
        if !args.hidden
            && let Some(name) = entry.file_name().to_str()
            && name.starts_with('.')
        {
            continue;
        }

        let rel = relative_path(entry.path(), &base);
        if matcher.is_match(&rel) {
            let metadata = entry
                .metadata()
                .unwrap_or_else(|_| std::fs::metadata(entry.path()).expect("cannot read metadata"));
            results.push((entry.path().to_path_buf(), metadata));
        }
    }

    // Sort
    match args.sort {
        SortOrder::Name => results.sort_by(|(a, _), (b, _)| a.cmp(b)),
        SortOrder::Modified => results.sort_by(|(_, a), (_, b)| {
            let ta = a.modified().unwrap_or(std::time::SystemTime::UNIX_EPOCH);
            let tb = b.modified().unwrap_or(std::time::SystemTime::UNIX_EPOCH);
            ta.cmp(&tb)
        }),
        SortOrder::Size => results.sort_by(|(_, a), (_, b)| a.len().cmp(&b.len())),
        SortOrder::None => {}
    }

    if results.is_empty() {
        return Ok(ExitCode::from(1));
    }

    let stdout = io::stdout();
    let handle = stdout.lock();
    let mut writer = BoundedWriter::new(handle, args.limit);

    for (path, _) in &results {
        let rel = relative_path(path, &base);
        if !writer.write_line(&rel)? {
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
    fn test_should_skip_hidden() {
        assert!(should_skip(".git", false));
        assert!(should_skip(".hidden", false));
        assert!(!should_skip(".hidden", true));
        assert!(should_skip("node_modules", false));
        assert!(should_skip("node_modules", true)); // always skip junk dirs
    }

    #[test]
    fn test_glob_in_tempdir() {
        use std::io::Write;
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        std::fs::create_dir(&src).unwrap();
        std::fs::File::create(src.join("main.rs"))
            .unwrap()
            .write_all(b"fn main() {}")
            .unwrap();
        std::fs::File::create(src.join("lib.rs"))
            .unwrap()
            .write_all(b"// lib")
            .unwrap();
        std::fs::File::create(dir.path().join("README.md"))
            .unwrap()
            .write_all(b"# readme")
            .unwrap();

        let args = GlobArgs {
            pattern: "**/*.rs".to_string(),
            path: dir.path().to_path_buf(),
            entry_type: EntryType::File,
            max_depth: None,
            hidden: false,
            follow_links: false,
            sort: SortOrder::Name,
            limit: None,
        };

        let exit = run(&args).unwrap();
        assert_eq!(exit, ExitCode::SUCCESS);
    }

    #[test]
    fn test_glob_no_matches() {
        let dir = tempfile::tempdir().unwrap();
        let args = GlobArgs {
            pattern: "**/*.nonexistent".to_string(),
            path: dir.path().to_path_buf(),
            entry_type: EntryType::File,
            max_depth: None,
            hidden: false,
            follow_links: false,
            sort: SortOrder::Name,
            limit: None,
        };
        let exit = run(&args).unwrap();
        assert_eq!(exit, ExitCode::from(1));
    }
}
