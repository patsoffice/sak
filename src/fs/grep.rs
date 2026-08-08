use crate::output::Outcome;
use std::io::{self, BufRead, BufReader};
use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::Args;
use globset::Glob;
use regex::Regex;
use walkdir::WalkDir;

use super::STDIN_LABEL;
use crate::output::{BoundedWriter, Limit, is_binary, line_number_width, relative_path};

#[derive(Args)]
#[command(
    about = "Search file contents with regex",
    long_about = "Search file contents using regular expressions.\n\n\
        Recursively searches files for lines matching the given regex pattern. \
        Supports multiline matching where . matches newlines.\n\n\
        Pass '-' as a path to read from stdin (matches grep/ripgrep), enabling \
        pipelines like 'sak json query … | sak fs grep -'. Stdin can be mixed \
        with file/directory paths; matches from stdin are labelled \
        '(standard input)'.\n\n\
        Counting: '-c' prints matches per file and '--total' prints one \
        grand total across every file searched — prefer either over counting \
        the default output's lines, which also carries filename headings. \
        '--no-heading' is the parse-friendly mode: one 'path:line:text' row \
        per match, no headings and no '--' separators, so every stdout line \
        is exactly one match.",
    after_help = "\
Examples:
  sak fs grep 'fn main' src/                       Find 'fn main' in src/
  sak fs grep -i 'error' /var/log/app.log          Case-insensitive search
  sak fs grep -U 'struct \\w+\\s*\\{[^}]*\\}' .    Multiline: find struct bodies
  sak fs grep -l 'TODO' --glob '**/*.rs'           List Rust files with TODOs
  sak fs grep -c 'error' logs/                     Count matches per file
  sak fs grep --total 'unwrap()' src/              One total across all files
  sak fs grep --no-heading 'TODO' src/             path:line:text rows, no decoration
  sak fs grep -C 3 'panic' src/                    Show 3 lines of context
  some-cmd | sak fs grep 'pattern' -                Search piped stdin"
)]
pub struct GrepArgs {
    /// Regex pattern to search for
    pub pattern: String,

    /// Files or directories to search ('-' reads stdin)
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

    /// Print one grand-total match count across every file searched
    #[arg(long, conflicts_with_all = ["count", "files_only"])]
    pub total: bool,

    /// Stop after N matches per file
    #[arg(short = 'm', long = "max-count")]
    pub max_count: Option<usize>,

    /// Show line numbers (enabled by default)
    //
    // Deliberately a bare `SetTrue` flag rather than a value-taking bool like
    // `--heading`: `-n` is universal grep muscle memory, and giving it an
    // optional value would turn `sak fs grep -n 'fn main' src/` into an
    // "invalid value" error. Use `--no-line-numbers` to switch it off.
    #[arg(short = 'n', long = "line-number", conflicts_with = "no_line_numbers")]
    pub line_number: bool,

    /// Omit line numbers from output
    #[arg(long = "no-line-numbers")]
    pub no_line_numbers: bool,

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
    //
    // `num_args = 0..=1` + `default_missing_value` so every documented form
    // parses: bare `--heading`, `--heading false`, and `--heading=false`. As a
    // plain `SetTrue` flag (what it used to be) the value forms silently leaked
    // `false` into `paths` and searched a file by that name — the no-heading
    // rendering below was unreachable from the CLI entirely.
    #[arg(
        long,
        action = clap::ArgAction::Set,
        num_args = 0..=1,
        default_value = "true",
        default_missing_value = "true"
    )]
    pub heading: bool,

    /// Parse-friendly output: `path:line:text` rows, no filename headings and
    /// no `--` separators, so every stdout line is exactly one match
    #[arg(long = "no-heading")]
    pub no_heading: bool,
}

impl GrepArgs {
    /// Whether to group matches under a filename heading. `--no-heading` is the
    /// ergonomic spelling of `--heading false`; either one turns grouping off.
    fn use_heading(&self) -> bool {
        self.heading && !self.no_heading
    }

    /// Whether to prefix each row with its line number. `-n` is accepted for
    /// grep compatibility but numbers are already on; clap makes it mutually
    /// exclusive with `--no-line-numbers`, so the two can't disagree.
    fn use_line_numbers(&self) -> bool {
        self.line_number || !self.no_line_numbers
    }
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
        // '-' is the stdin sentinel, handled separately in run().
        if path.as_os_str() == "-" {
            continue;
        }
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
    search_reader_lines(reader, re, max_count, before_ctx, after_ctx, path.clone())
        .with_context(|| format!("error reading: {}", path.display()))
}

fn search_reader_lines<R: BufRead>(
    reader: R,
    re: &Regex,
    max_count: Option<usize>,
    before_ctx: usize,
    after_ctx: usize,
    label: PathBuf,
) -> Result<Option<MatchResult>> {
    let lines: Vec<String> = reader
        .lines()
        .collect::<Result<Vec<_>, io::Error>>()
        .context("error reading input")?;

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

    // A `--` separator marks a gap between two *context blocks*. With no
    // context requested there are no blocks to delimit — every emitted line is
    // a match, and the line numbers already show the gaps — so grep and
    // ripgrep print nothing there, and so do we. Emitting them anyway is what
    // made `sak fs grep … | sak fs wc -l` overcount (sak-llm-fs-grep-count-format-hhva).
    let separators = before_ctx > 0 || after_ctx > 0;

    for &match_idx in &match_line_nums {
        let ctx_start = match_idx.saturating_sub(before_ctx);
        let ctx_end = (match_idx + after_ctx).min(lines.len() - 1);

        // Add separator if there's a gap
        if separators
            && let Some(last) = last_shown
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
        path: label,
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
    search_content_multiline(&content, re, max_count, path.clone())
}

fn search_reader_multiline<R: io::Read>(
    mut reader: R,
    re: &Regex,
    max_count: Option<usize>,
    label: PathBuf,
) -> Result<Option<MatchResult>> {
    let mut content = String::new();
    reader
        .read_to_string(&mut content)
        .context("error reading input")?;
    search_content_multiline(&content, re, max_count, label)
}

fn search_content_multiline(
    content: &str,
    re: &Regex,
    max_count: Option<usize>,
    label: PathBuf,
) -> Result<Option<MatchResult>> {
    let mut matches_found: Vec<LineMatch> = Vec::new();
    let mut count = 0;

    for mat in re.find_iter(content) {
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
        path: label,
        matches: matches_found,
        count,
    }))
}

/// Emit one file's (or stdin's) matches. Returns `Ok(false)` when the output
/// limit has been reached and the caller should stop emitting further results.
fn emit_result(
    result: &MatchResult,
    label: &str,
    args: &GrepArgs,
    multi_file: bool,
    first_file: &mut bool,
    writer: &mut BoundedWriter<'_>,
) -> Result<bool> {
    if args.files_only {
        return Ok(writer.write_line(label)?);
    }

    if args.count {
        return if multi_file {
            Ok(writer.write_line(&format!("{}:{}", label, result.count))?)
        } else {
            Ok(writer.write_line(&format!("{}", result.count))?)
        };
    }

    // Regular output
    let line_numbers = args.use_line_numbers();
    if args.use_heading() {
        if !*first_file {
            writer.write_decoration("")?;
        }
        if multi_file {
            writer.write_decoration(label)?;
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
                let prefix = if line_numbers {
                    let sep = if m.is_context { "-" } else { ":" };
                    format!("{:>width$}{}{}", m.line_num, sep, m.content, width = width)
                } else {
                    m.content.clone()
                };
                if !writer.write_line(&prefix)? {
                    return Ok(false);
                }
            }
        }
    } else {
        // No heading: every row carries its own path and line number, so it is
        // self-describing and a `--` separator would add nothing — dropping it
        // is what makes this mode safe to pipe into a line counter. Context
        // rows use grep's `path-line-text` form so they stay distinguishable
        // from matches (`path:line:text`).
        for m in &result.matches {
            if m.is_separator {
                continue;
            }
            let line = if line_numbers {
                let sep = if m.is_context { '-' } else { ':' };
                format!("{}{}{}{}{}", label, sep, m.line_num, sep, m.content)
            } else {
                let sep = if m.is_context { '-' } else { ':' };
                format!("{}{}{}", label, sep, m.content)
            };
            if !writer.write_line(&line)? {
                return Ok(false);
            }
        }
    }

    *first_file = false;
    Ok(true)
}

pub fn run(args: &GrepArgs) -> Result<Outcome> {
    let re = build_regex(&args.pattern, args.ignore_case, args.word, args.multiline)?;
    let files = collect_files(args)?;
    let read_stdin = args.paths.iter().any(|p| p.as_os_str() == "-");

    let before_ctx = args.before_context.or(args.context).unwrap_or(0);
    let after_ctx = args.after_context.or(args.context).unwrap_or(0);

    // stdin counts as one source when deciding whether to label output by source.
    let multi_file = files.len() + usize::from(read_stdin) > 1;
    let base = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));

    let stdout = io::stdout();
    let handle = stdout.lock();
    // `--total` emits exactly one line, so `--limit` (which bounds match rows)
    // has nothing to bound — and a `--limit 0` from a wrapper script must not
    // swallow the answer.
    let limit = if args.total {
        Limit::None
    } else {
        Limit::from(args.limit)
    };
    let mut writer = BoundedWriter::new(handle, limit);
    let mut any_match = false;
    let mut first_file = true;
    let mut total = 0usize;

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
        total += result.count;
        if args.total {
            continue;
        }
        let rel = relative_path(&result.path, &base);
        if !emit_result(
            &result,
            &rel,
            args,
            multi_file,
            &mut first_file,
            &mut writer,
        )? {
            writer.flush()?;
            return Ok(Outcome::Found);
        }
    }

    // stdin is searched after file/directory paths.
    if read_stdin {
        let stdin = io::stdin();
        let result = if args.multiline {
            search_reader_multiline(
                stdin.lock(),
                &re,
                args.max_count,
                PathBuf::from(STDIN_LABEL),
            )?
        } else {
            search_reader_lines(
                stdin.lock(),
                &re,
                args.max_count,
                before_ctx,
                after_ctx,
                PathBuf::from(STDIN_LABEL),
            )?
        };

        if let Some(result) = result {
            any_match = true;
            total += result.count;
            if !args.total {
                emit_result(
                    &result,
                    STDIN_LABEL,
                    args,
                    multi_file,
                    &mut first_file,
                    &mut writer,
                )?;
            }
        }
    }

    // The count *is* the answer, so print it even when it's zero — unlike a
    // match listing, an empty one would leave the caller unable to tell "no
    // matches" from "command produced nothing". The exit code still follows
    // the usual convention (0 = found, 1 = none).
    if args.total {
        writer.write_line(&total.to_string())?;
    }

    writer.flush()?;

    if any_match {
        Ok(Outcome::Found)
    } else {
        Ok(Outcome::NotFound)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    /// Wrapper so we can drive `GrepArgs` through clap parsing in tests.
    #[derive(Parser)]
    struct GrepCli {
        #[command(flatten)]
        args: GrepArgs,
    }

    fn parse(argv: &[&str]) -> GrepArgs {
        GrepCli::try_parse_from(argv).unwrap().args
    }

    #[test]
    fn heading_can_actually_be_switched_off() {
        // Regression: as a bare `SetTrue` flag, `--heading false` left "false"
        // in `paths` — grep then searched a file by that name and still printed
        // headings, making the no-heading renderer unreachable from the CLI.
        for argv in [
            &["grep", "pat", "src/", "--heading", "false"][..],
            &["grep", "--heading=false", "pat", "src/"][..],
            &["grep", "--no-heading", "pat", "src/"][..],
        ] {
            let args = parse(argv);
            assert!(!args.use_heading(), "{argv:?}");
            assert_eq!(args.paths, [PathBuf::from("src/")], "{argv:?}");
        }
        // ...and the default is still on.
        assert!(parse(&["grep", "pat", "src/"]).use_heading());
    }

    #[test]
    fn line_numbers_can_actually_be_switched_off() {
        assert!(parse(&["grep", "pat", "src/"]).use_line_numbers());
        let args = parse(&["grep", "pat", "src/", "--no-line-numbers"]);
        assert!(!args.use_line_numbers());
        assert_eq!(args.paths, [PathBuf::from("src/")]);
        // `-n` keeps grep's bare-flag shape: the pattern must not be eaten as
        // a value for it, which is why it is not a value-taking bool.
        let args = parse(&["grep", "-n", "pat", "src/"]);
        assert!(args.use_line_numbers());
        assert_eq!(args.pattern, "pat");
    }

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
        // With no context requested there is nothing to separate, so the
        // emitted rows are exactly the matches — one output line each. Callers
        // count `sak fs grep … | sak fs wc -l` and must get the match count.
        assert_eq!(result.matches.len(), 2);
        assert!(result.matches.iter().all(|m| !m.is_separator));
        assert_eq!(result.matches[0].line_num, 3);
        assert_eq!(result.matches[1].line_num, 5);
    }

    #[test]
    fn separators_return_once_context_is_requested() {
        // The gap between two context blocks is real information, so `--` comes
        // back as soon as -A/-B/-C is in play. Lines 3 and 9 are far enough
        // apart that their 1-line context blocks don't touch.
        let input = "a\nb\nMATCH\nd\ne\nf\ng\nh\nMATCH\nj\n";
        let re = Regex::new("MATCH").unwrap();
        let result = search_reader_lines(
            io::Cursor::new(input),
            &re,
            None,
            1,
            1,
            PathBuf::from(STDIN_LABEL),
        )
        .unwrap()
        .unwrap();
        assert_eq!(result.count, 2);
        assert_eq!(result.matches.iter().filter(|m| m.is_separator).count(), 1);
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
    fn test_search_reader_lines_stdin() {
        let input = "line one\nmatch here\nline three\nanother match\n";
        let re = Regex::new("match").unwrap();
        let result = search_reader_lines(
            io::Cursor::new(input),
            &re,
            None,
            0,
            0,
            PathBuf::from(STDIN_LABEL),
        )
        .unwrap()
        .unwrap();
        assert_eq!(result.count, 2);
        assert_eq!(result.path, PathBuf::from(STDIN_LABEL));
        let non_sep: Vec<_> = result.matches.iter().filter(|m| !m.is_separator).collect();
        assert_eq!(non_sep[0].line_num, 2);
        assert_eq!(non_sep[1].line_num, 4);
    }

    #[test]
    fn test_search_reader_lines_no_match() {
        let re = Regex::new("absent").unwrap();
        let result = search_reader_lines(
            io::Cursor::new("nothing here\n"),
            &re,
            None,
            0,
            0,
            PathBuf::from(STDIN_LABEL),
        )
        .unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_search_reader_multiline_stdin() {
        let input = "struct Foo {\n    x: i32,\n}\n";
        let re = Regex::new(r"(?s)struct \w+\s*\{[^}]*\}").unwrap();
        let result = search_reader_multiline(
            io::Cursor::new(input),
            &re,
            None,
            PathBuf::from(STDIN_LABEL),
        )
        .unwrap()
        .unwrap();
        assert_eq!(result.count, 1);
        assert_eq!(result.path, PathBuf::from(STDIN_LABEL));
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
