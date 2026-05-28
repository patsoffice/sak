use crate::output::Outcome;
use std::io;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use clap::{Args, ValueEnum};
use globset::{Glob, GlobSet, GlobSetBuilder};

use super::{is_hidden_file, pruned_walk};
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
        Recursively searches directories for entries matching any of the given glob \
        patterns (OR semantics — an entry matching any pattern qualifies). \
        Supports ** for recursive matching and {a,b} alternation.\n\n\
        Multiple patterns may be given positionally. An optional trailing argument that \
        is an existing directory (and is not itself a glob) sets the search root; \
        otherwise the root defaults to the current directory.",
    after_help = "\
Examples:
  sak fs glob '**/*.rs'                    Find all Rust files
  sak fs glob '**/*.rs' src/               Find Rust files under src/
  sak fs glob '*.rs' '*.toml'              Find Rust OR TOML files
  sak fs glob flake.nix shell.nix .        Probe for any nix dev-shell hint
  sak fs glob 'src/{main,lib}.rs'          Find specific files
  sak fs glob '**/*' --type dir            List all directories
  sak fs glob '**/*.log' --max-depth 2     Shallow search for log files"
)]
pub struct GlobArgs {
    /// Glob patterns (e.g., "**/*.rs", "src/{main,lib}.rs"). Multiple patterns
    /// match with OR semantics. An optional trailing existing-directory argument
    /// sets the search root (defaults to ".").
    #[arg(required = true, num_args = 1..)]
    pub patterns: Vec<String>,

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

fn matches_type(entry: &walkdir::DirEntry, entry_type: &EntryType) -> bool {
    let ft = entry.file_type();
    match entry_type {
        EntryType::File => ft.is_file(),
        EntryType::Dir => ft.is_dir(),
        EntryType::Symlink => ft.is_symlink(),
    }
}

/// Split the positional list into (patterns, search root).
///
/// With two or more positionals, a trailing argument that is *not* a glob
/// (no `*`/`?`/`[`/`{`) and names an existing directory is peeled off as the
/// search root; otherwise the root defaults to ".". A lone positional is
/// always a pattern, preserving `sak fs glob '**/*.rs'`.
fn split_patterns_path(positionals: &[String]) -> (Vec<String>, PathBuf) {
    if positionals.len() > 1 {
        let last = &positionals[positionals.len() - 1];
        let looks_like_glob = last.contains(['*', '?', '[', '{']);
        if !looks_like_glob && Path::new(last).is_dir() {
            let (pats, path) = positionals.split_at(positionals.len() - 1);
            return (pats.to_vec(), PathBuf::from(&path[0]));
        }
    }
    (positionals.to_vec(), PathBuf::from("."))
}

pub fn run(args: &GlobArgs) -> Result<Outcome> {
    let (patterns, path) = split_patterns_path(&args.patterns);

    let mut builder = GlobSetBuilder::new();
    for pattern in &patterns {
        let glob =
            Glob::new(pattern).with_context(|| format!("invalid glob pattern: {}", pattern))?;
        builder.add(glob);
    }
    let matcher: GlobSet = builder.build().context("failed to build glob set")?;

    let base = path
        .canonicalize()
        .with_context(|| format!("cannot access directory: {}", path.display()))?;

    let mut results: Vec<(PathBuf, std::fs::Metadata)> = Vec::new();

    let iter = pruned_walk(&base, args.hidden, args.follow_links, args.max_depth);

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
        if is_hidden_file(&entry, args.hidden) {
            continue;
        }

        let rel = relative_path(entry.path(), &base);
        if matcher.is_match(&rel) {
            // `entry.metadata()` uses walkdir's cached stat; if that fails
            // (e.g. a symlink walkdir didn't follow), fall back to a direct
            // stat. A walkdir entry can still fail to stat on a TOCTOU race
            // or permission change between dir-read and stat, so surface that
            // as a normal sak error (exit 2) rather than panicking.
            let metadata = match entry.metadata() {
                Ok(m) => m,
                Err(_) => std::fs::metadata(entry.path()).with_context(|| {
                    format!("cannot read metadata for {}", entry.path().display())
                })?,
            };
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
        SortOrder::Size => results.sort_by_key(|(_, a)| a.len()),
        SortOrder::None => {}
    }

    if results.is_empty() {
        return Ok(Outcome::NotFound);
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
    Ok(Outcome::Found)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_should_skip_hidden() {
        use super::super::should_skip;
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
            patterns: vec!["**/*.rs".to_string(), dir.path().display().to_string()],
            entry_type: EntryType::File,
            max_depth: None,
            hidden: false,
            follow_links: false,
            sort: SortOrder::Name,
            limit: None,
        };

        let exit = run(&args).unwrap();
        assert_eq!(exit, Outcome::Found);
    }

    #[test]
    fn test_glob_no_matches() {
        let dir = tempfile::tempdir().unwrap();
        let args = GlobArgs {
            patterns: vec![
                "**/*.nonexistent".to_string(),
                dir.path().display().to_string(),
            ],
            entry_type: EntryType::File,
            max_depth: None,
            hidden: false,
            follow_links: false,
            sort: SortOrder::Name,
            limit: None,
        };
        let exit = run(&args).unwrap();
        assert_eq!(exit, Outcome::NotFound);
    }

    #[test]
    fn test_split_patterns_path() {
        // Lone positional is always a pattern; root defaults to ".".
        let (pats, path) = split_patterns_path(&["**/*.rs".to_string()]);
        assert_eq!(pats, vec!["**/*.rs".to_string()]);
        assert_eq!(path, PathBuf::from("."));

        // A trailing existing directory is peeled off as the root.
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path().display().to_string();
        let (pats, path) =
            split_patterns_path(&["*.rs".to_string(), "*.toml".to_string(), d.clone()]);
        assert_eq!(pats, vec!["*.rs".to_string(), "*.toml".to_string()]);
        assert_eq!(path, PathBuf::from(&d));

        // A trailing glob is a pattern, not a path — root stays ".".
        let (pats, path) = split_patterns_path(&["*.rs".to_string(), "*.toml".to_string()]);
        assert_eq!(pats, vec!["*.rs".to_string(), "*.toml".to_string()]);
        assert_eq!(path, PathBuf::from("."));
    }

    #[test]
    fn test_glob_multiple_patterns() {
        use std::io::Write;
        let dir = tempfile::tempdir().unwrap();
        for name in ["a.rs", "b.toml", "c.md"] {
            std::fs::File::create(dir.path().join(name))
                .unwrap()
                .write_all(b"x")
                .unwrap();
        }

        let (patterns, path) = split_patterns_path(&[
            "*.rs".to_string(),
            "*.toml".to_string(),
            dir.path().display().to_string(),
        ]);
        assert_eq!(path, dir.path());

        // Build the same set `run` builds and assert OR-semantics matching.
        let mut builder = GlobSetBuilder::new();
        for p in &patterns {
            builder.add(Glob::new(p).unwrap());
        }
        let set = builder.build().unwrap();
        assert!(set.is_match("a.rs"));
        assert!(set.is_match("b.toml"));
        assert!(!set.is_match("c.md"));
    }
}
