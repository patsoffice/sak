use std::io::{self, BufRead, BufReader};
use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{Context, Result};
use clap::Args;
use globset::Glob;
use regex::Regex;
use walkdir::WalkDir;

use crate::output::{BoundedWriter, is_binary, line_number_width, relative_path};

#[derive(Args)]
#[command(
    about = "Search file contents with regex",
    long_about = "Search file contents using regular expressions.\n\n\
        Recursively searches files for lines matching the given regex pattern. \
        Supports multiline matching where . matches newlines.",
    after_help = "\
Examples:
  sak fs grep 'fn main' src/                       Find 'fn main' in src/
  sak fs grep -i 'error' /var/log/app.log          Case-insensitive search
  sak fs grep -U 'struct \\w+\\s*\\{[^}]*\\}' .    Multiline: find struct bodies
  sak fs grep -l 'TODO' --glob '**/*.rs'           List Rust files with TODOs
  sak fs grep -c 'error' logs/                     Count matches per file
  sak fs grep -C 3 'panic' src/                    Show 3 lines of context"
)]
pub struct GrepArgs {
    /// Regex pattern to search for
    pub pattern: String,

    /// Files or directories to search
    #[arg(default_value = ".")]
    pub paths: Vec<PathBuf>,

    /// Case-insensitive matching
    #[arg(short = 'i', long = "ignore-case")]
    pub ignore_case: bool,

    /// Match whole words only
    #[arg(short = 'w', long = "word")]
    pub word: bool,

    /// Print only file paths that contain matches
    #[arg(short = 'l', long = "files-only")]
    pub files_only: bool,

    /// Print match count per file
    #[arg(short = 'c', long = "count")]
    pub count: bool,

    /// Stop after N matches per file
    #[arg(short = 'm', long = "max-count")]
    pub max_count: Option<usize>,

    /// Show line numbers (enabled by default)
    #[arg(short = 'n', long = "line-number", default_value = "true")]
    pub line_number: bool,

    /// Lines of context around each match
    #[arg(short = 'C', long = "context")]
    pub context: Option<usize>,

    /// Lines of context before each match
    #[arg(short = 'B', long = "before-context")]
    pub before_context: Option<usize>,

    /// Lines of context after each match
    #[arg(short = 'A', long = "after-context")]
    pub after_context: Option<usize>,

    /// Enable multiline matching (. matches newline)
    #[arg(short = 'U', long = "multiline")]
    pub multiline: bool,

    /// Only search files matching this glob pattern
    #[arg(short = 'g', long = "glob")]
    pub file_glob: Option<String>,

    /// Only search files with this extension
    #[arg(long = "type")]
    pub file_type: Option<String>,

    /// Include hidden files and directories
    #[arg(short = 'H', long)]
    pub hidden: bool,

    /// Maximum directory depth to recurse
    #[arg(long)]
    pub max_depth: Option<usize>,

    /// Maximum total matches to return
    #[arg(long)]
    pub limit: Option<usize>,

    /// Group matches by file (default: true)
    #[arg(long, default_value = "true")]
    pub heading: bool,
}

const SKIP_DIRS: &[&str] = &[".git", "target", "node_modules", "__pycache__", ".venv"];

fn build_regex(pattern: &str, ignore_case: bool, word: bool, multiline: bool) -> Result<Regex> {
    let mut pat = pattern.to_string();
    if word {
        pat = format!(r"\b{}\b", pat);
    }
    let mut prefix = String::new();
    if ignore_case {
        prefix.push_str("(?i)");
    }
    if multiline {
        prefix.push_str("(?s)");
    }
    let full = format!("{}{}", prefix, pat);
    Regex::new(&full).with_context(|| format!("invalid regex: {}", pattern))
}

fn collect_files(args: &GrepArgs) -> Result<Vec<PathBuf>> {
    let glob_matcher = args
        .file_glob
        .as_ref()
        .map(|g| Glob::new(g).with_context(|| format!("invalid glob: {}", g)))
        .transpose()?
        .map(|g| g.compile_matcher());

    let mut files = Vec::new();

    for path in &args.paths {
        if path.is_file() {
            files.push(path.clone());
            continue;
        }

        let mut walker = WalkDir::new(path).follow_links(false);
        if let Some(depth) = args.max_depth {
            walker = walker.max_depth(depth);
        }

        let hidden = args.hidden;
        let iter = walker.into_iter().filter_entry(move |e| {
            if e.depth() > 0
                && e.file_type().is_dir()
                && let Some(name) = e.file_name().to_str()
            {
                if SKIP_DIRS.contains(&name) {
                    return false;
                }
                if !hidden && name.starts_with('.') {
                    return false;
                }
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

            if !entry.file_type().is_file() {
                continue;
            }

            if !args.hidden
                && let Some(name) = entry.file_name().to_str()
                && name.starts_with('.')
            {
                continue;
            }

            // Filter by extension
            if let Some(ref ext) = args.file_type
                && entry.path().extension().and_then(|e| e.to_str()) != Some(ext.as_str())
            {
                continue;
            }

            // Filter by glob
            if let Some(ref matcher) = glob_matcher {
                let rel = relative_path(entry.path(), path);
                if !matcher.is_match(&rel) {
                    continue;
                }
            }

            files.push(entry.path().to_path_buf());
        }
    }

    files.sort();
    Ok(files)
}

struct MatchResult {
    path: PathBuf,
    matches: Vec<LineMatch>,
    count: usize,
}

struct LineMatch {
    line_num: usize,
    content: String,
    is_context: bool,
    is_separator: bool,
}

fn search_file_lines(
    path: &PathBuf,
    re: &Regex,
    max_count: Option<usize>,
    before_ctx: usize,
    after_ctx: usize,
) -> Result<Option<MatchResult>> {
    if is_binary(path).unwrap_or(false) {
        return Ok(None);
    }

    let file =
        std::fs::File::open(path).with_context(|| format!("cannot open: {}", path.display()))?;
    let reader = BufReader::new(file);
    let lines: Vec<String> = reader
        .lines()
        .collect::<Result<Vec<_>, io::Error>>()
        .with_context(|| format!("error reading: {}", path.display()))?;

    let mut match_line_nums: Vec<usize> = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        if re.is_match(line) {
            match_line_nums.push(i);
            if let Some(max) = max_count
                && match_line_nums.len() >= max
            {
                break;
            }
        }
    }

    if match_line_nums.is_empty() {
        return Ok(None);
    }

    let count = match_line_nums.len();

    // Build output with context
    let mut output_lines: Vec<LineMatch> = Vec::new();
    let mut shown: std::collections::HashSet<usize> = std::collections::HashSet::new();
    let mut last_shown: Option<usize> = None;

    for &match_idx in &match_line_nums {
        let ctx_start = match_idx.saturating_sub(before_ctx);
        let ctx_end = (match_idx + after_ctx).min(lines.len() - 1);

        // Add separator if there's a gap
        if let Some(last) = last_shown
            && ctx_start > last + 1
        {
            output_lines.push(LineMatch {
                line_num: 0,
                content: "--".to_string(),
                is_context: false,
                is_separator: true,
            });
        }

        #[allow(clippy::needless_range_loop)]
        for i in ctx_start..=ctx_end {
            if shown.contains(&i) {
                continue;
            }
            shown.insert(i);
            output_lines.push(LineMatch {
                line_num: i + 1, // 1-based
                content: lines[i].clone(),
                is_context: i != match_idx,
                is_separator: false,
            });
            last_shown = Some(i);
        }
    }

    Ok(Some(MatchResult {
        path: path.clone(),
        matches: output_lines,
        count,
    }))
}

fn search_file_multiline(
    path: &PathBuf,
    re: &Regex,
    max_count: Option<usize>,
) -> Result<Option<MatchResult>> {
    if is_binary(path).unwrap_or(false) {
        return Ok(None);
    }

    let content = std::fs::read_to_string(path)
        .with_context(|| format!("cannot read: {}", path.display()))?;

    let mut matches_found: Vec<LineMatch> = Vec::new();
    let mut count = 0;

    for mat in re.find_iter(&content) {
        count += 1;
        // Find line number of match start
        let line_num = content[..mat.start()].matches('\n').count() + 1;
        let matched_text = mat.as_str();

        // For multiline matches, show all lines of the match
        for (i, line) in matched_text.lines().enumerate() {
            matches_found.push(LineMatch {
                line_num: line_num + i,
                content: line.to_string(),
                is_context: false,
                is_separator: false,
            });
        }

        if let Some(max) = max_count
            && count >= max
        {
            break;
        }

        // Add separator between matches
        matches_found.push(LineMatch {
            line_num: 0,
            content: "--".to_string(),
            is_context: false,
            is_separator: true,
        });
    }

    // Remove trailing separator
    if matches_found.last().is_some_and(|m| m.is_separator) {
        matches_found.pop();
    }

    if count == 0 {
        return Ok(None);
    }

    Ok(Some(MatchResult {
        path: path.clone(),
        matches: matches_found,
        count,
    }))
}

pub fn run(args: &GrepArgs) -> Result<ExitCode> {
    let re = build_regex(&args.pattern, args.ignore_case, args.word, args.multiline)?;
    let files = collect_files(args)?;

    let before_ctx = args.before_context.or(args.context).unwrap_or(0);
    let after_ctx = args.after_context.or(args.context).unwrap_or(0);

    let multi_file = files.len() > 1;
    let base = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));

    let stdout = io::stdout();
    let handle = stdout.lock();
    let mut writer = BoundedWriter::new(handle, args.limit);
    let mut any_match = false;
    let mut first_file = true;

    for file_path in &files {
        let result = if args.multiline {
            search_file_multiline(file_path, &re, args.max_count)?
        } else {
            search_file_lines(file_path, &re, args.max_count, before_ctx, after_ctx)?
        };

        let result = match result {
            Some(r) => r,
            None => continue,
        };

        any_match = true;
        let rel = relative_path(&result.path, &base);

        if args.files_only {
            if !writer.write_line(&rel)? {
                break;
            }
            continue;
        }

        if args.count {
            if multi_file {
                if !writer.write_line(&format!("{}:{}", rel, result.count))? {
                    break;
                }
            } else if !writer.write_line(&format!("{}", result.count))? {
                break;
            }
            continue;
        }

        // Regular output
        if args.heading {
            if !first_file {
                writer.write_decoration("")?;
            }
            if multi_file {
                writer.write_decoration(&rel)?;
            }
            let max_ln = result
                .matches
                .iter()
                .filter(|m| !m.is_separator)
                .map(|m| m.line_num)
                .max()
                .unwrap_or(1);
            let width = line_number_width(max_ln);

            for m in &result.matches {
                if m.is_separator {
                    writer.write_decoration(&m.content)?;
                } else {
                    let prefix = if args.line_number {
                        let sep = if m.is_context { "-" } else { ":" };
                        format!("{:>width$}{}{}", m.line_num, sep, m.content, width = width)
                    } else {
                        m.content.clone()
                    };
                    if !writer.write_line(&prefix)? {
                        writer.flush()?;
                        return Ok(ExitCode::SUCCESS);
                    }
                }
            }
        } else {
            // No heading: file:line:content
            for m in &result.matches {
                if m.is_separator {
                    writer.write_decoration("--")?;
                } else {
                    let line = if args.line_number {
                        format!("{}:{}:{}", rel, m.line_num, m.content)
                    } else {
                        format!("{}:{}", rel, m.content)
                    };
                    if !writer.write_line(&line)? {
                        writer.flush()?;
                        return Ok(ExitCode::SUCCESS);
                    }
                }
            }
        }

        first_file = false;
    }

    writer.flush()?;

    if any_match {
        Ok(ExitCode::SUCCESS)
    } else {
        Ok(ExitCode::from(1))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_regex_basic() {
        let re = build_regex("hello", false, false, false).unwrap();
        assert!(re.is_match("hello world"));
        assert!(!re.is_match("HELLO world"));
    }

    #[test]
    fn test_build_regex_case_insensitive() {
        let re = build_regex("hello", true, false, false).unwrap();
        assert!(re.is_match("HELLO world"));
    }

    #[test]
    fn test_build_regex_word_boundary() {
        let re = build_regex("main", false, true, false).unwrap();
        assert!(re.is_match("fn main() {"));
        assert!(!re.is_match("domain"));
    }

    #[test]
    fn test_build_regex_multiline() {
        let re = build_regex("a.b", false, false, true).unwrap();
        assert!(re.is_match("a\nb"));
    }

    #[test]
    fn test_search_file_lines() {
        use std::io::Write;
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("test.txt");
        {
            let mut f = std::fs::File::create(&file_path).unwrap();
            writeln!(f, "line one").unwrap();
            writeln!(f, "line two").unwrap();
            writeln!(f, "match here").unwrap();
            writeln!(f, "line four").unwrap();
            writeln!(f, "another match").unwrap();
        }

        let re = Regex::new("match").unwrap();
        let result = search_file_lines(&file_path, &re, None, 0, 0)
            .unwrap()
            .unwrap();
        assert_eq!(result.count, 2);
        // 2 matches + 1 separator between non-adjacent matches
        assert_eq!(result.matches.len(), 3);
        assert_eq!(result.matches[0].line_num, 3);
        assert!(result.matches[1].is_separator);
        assert_eq!(result.matches[2].line_num, 5);
    }

    #[test]
    fn test_search_file_with_context() {
        use std::io::Write;
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("test.txt");
        {
            let mut f = std::fs::File::create(&file_path).unwrap();
            writeln!(f, "line 1").unwrap();
            writeln!(f, "line 2").unwrap();
            writeln!(f, "MATCH").unwrap();
            writeln!(f, "line 4").unwrap();
            writeln!(f, "line 5").unwrap();
        }

        let re = Regex::new("MATCH").unwrap();
        let result = search_file_lines(&file_path, &re, None, 1, 1)
            .unwrap()
            .unwrap();
        assert_eq!(result.count, 1);
        // Should include line 2 (before), line 3 (match), line 4 (after)
        let non_sep: Vec<_> = result.matches.iter().filter(|m| !m.is_separator).collect();
        assert_eq!(non_sep.len(), 3);
        assert_eq!(non_sep[0].line_num, 2);
        assert!(non_sep[0].is_context);
        assert_eq!(non_sep[1].line_num, 3);
        assert!(!non_sep[1].is_context);
        assert_eq!(non_sep[2].line_num, 4);
        assert!(non_sep[2].is_context);
    }

    #[test]
    fn test_search_file_multiline() {
        use std::io::Write;
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("test.txt");
        {
            let mut f = std::fs::File::create(&file_path).unwrap();
            writeln!(f, "struct Foo {{").unwrap();
            writeln!(f, "    x: i32,").unwrap();
            writeln!(f, "}}").unwrap();
        }

        let re = Regex::new(r"(?s)struct \w+\s*\{[^}]*\}").unwrap();
        let result = search_file_multiline(&file_path, &re, None)
            .unwrap()
            .unwrap();
        assert_eq!(result.count, 1);
    }

    #[test]
    fn test_search_skips_binary() {
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("binary.bin");
        std::fs::write(&file_path, b"\x00\x01\x02\x03").unwrap();

        let re = Regex::new("anything").unwrap();
        let result = search_file_lines(&file_path, &re, None, 0, 0).unwrap();
        assert!(result.is_none());
    }
}
