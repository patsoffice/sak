use crate::output::Outcome;
use std::fs::File;
use std::io::{self, BufRead, BufReader};
use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::Args;

use super::{is_stdin, require_piped_stdin};
use crate::output::{BoundedWriter, Limit, format_line_number, line_number_width};

/// Lines emitted when the caller doesn't pass `--limit`. Big enough to cover
/// most source files whole, small enough that reading a huge log by accident
/// stays cheap.
const DEFAULT_LIMIT: usize = 2000;

#[derive(Args)]
#[command(
    about = "Read file contents with line numbers",
    long_about = "Read file contents with line numbers.\n\n\
        Displays the contents of a file with line numbers for easy reference. \
        Supports reading specific line ranges and limiting output length. With \
        the file omitted, reads piped stdin (exit 2 if nothing is piped); '-' \
        always reads stdin (matches cat/grep).",
    after_help = "\
Examples:
  sak fs read src/main.rs                  Read entire file (up to 2000 lines)
  sak fs read src/main.rs -n 1-50          Read lines 1 through 50
  sak fs read src/main.rs -n 100-          Read from line 100 to end
  sak fs read src/main.rs -n -20           Read last 20 lines
  sak fs read src/main.rs --offset 10 --limit 5   Skip 10 lines, show 5
  sak fs read src/main.rs --exclude 75-357  Drop lines 75-357, emit the rest
  sak fs read data.txt --exclude 1-10,90-100   Drop several ranges
  some-cmd | sak fs read                   Read piped stdin (file omitted)"
)]
pub struct ReadArgs {
    /// Path to the file to read (omitted: piped stdin; '-' always reads stdin)
    pub file: Option<PathBuf>,

    /// Line range to read (e.g., "1-50", "100-", "-20")
    #[arg(short = 'n', long = "lines", allow_hyphen_values = true)]
    pub lines: Option<String>,

    /// Comma-separated line ranges to drop (e.g., "75-357", "1-10,90-100").
    /// Same syntax as -n per item; composes with -n and preserves the
    /// original line numbers of the lines that remain.
    #[arg(long = "exclude", value_name = "RANGES", allow_hyphen_values = true)]
    pub exclude: Option<String>,

    /// Omit line numbers from output
    #[arg(long = "no-line-numbers")]
    pub no_line_numbers: bool,

    /// Maximum number of lines to output [default: 2000]
    // Left as `Option` rather than given a clap `default_value` so the writer
    // can tell a caller-chosen cap (silent truncation) from the built-in one
    // (truncation announced on stderr) — see `crate::output::Limit`.
    #[arg(long, value_name = "N")]
    pub limit: Option<usize>,

    /// Number of lines to skip from the start (0-based)
    #[arg(long, default_value = "0")]
    pub offset: usize,
}

/// Parse a line range spec like "1-50", "100-", "-20".
/// Returns (start, end) as 1-based inclusive, where None means unbounded.
fn parse_line_range(spec: &str) -> Result<(Option<usize>, Option<usize>)> {
    let spec = spec.trim();
    if let Some(stripped) = spec.strip_prefix('-') {
        // "-20" means last 20 lines
        let n: usize = stripped
            .parse()
            .context("invalid line count in range spec")?;
        Ok((None, Some(n))) // Special: (None, Some(n)) means "last n lines"
    } else if let Some(stripped) = spec.strip_suffix('-') {
        let start: usize = stripped
            .parse()
            .context("invalid start line in range spec")?;
        Ok((Some(start), None))
    } else if let Some((a, b)) = spec.split_once('-') {
        let start: usize = a.parse().context("invalid start line in range spec")?;
        let end: usize = b.parse().context("invalid end line in range spec")?;
        Ok((Some(start), Some(end)))
    } else {
        // Single line number
        let n: usize = spec.parse().context("invalid line number")?;
        Ok((Some(n), Some(n)))
    }
}

/// Parse a comma-separated list of line range specs, each in the same
/// syntax [`parse_line_range`] accepts ("1-50", "100-", "-20", "42").
fn parse_exclude_ranges(spec: &str) -> Result<Vec<(Option<usize>, Option<usize>)>> {
    spec.split(',')
        .map(|item| {
            parse_line_range(item).with_context(|| format!("invalid range in --exclude: {item:?}"))
        })
        .collect()
}

/// True when 1-based `line_num` falls inside any of `ranges`. `total` is the
/// input's line count, which the "-N" (last-N-lines) form is relative to.
fn is_excluded(line_num: usize, total: usize, ranges: &[(Option<usize>, Option<usize>)]) -> bool {
    ranges.iter().any(|(start, end)| match (*start, *end) {
        (None, Some(n)) => line_num + n > total, // last n lines
        (Some(s), Some(e)) => line_num >= s && line_num <= e,
        (Some(s), None) => line_num >= s,
        (None, None) => false, // unreachable: parse_line_range never yields this
    })
}

pub fn run(args: &ReadArgs) -> Result<Outcome> {
    // An omitted file reads piped stdin; a file arg of "-" always reads stdin
    // (cat/grep convention); anything else is a path.
    let reader: Box<dyn BufRead> = match args.file.as_ref() {
        None => {
            require_piped_stdin()?;
            Box::new(io::stdin().lock())
        }
        Some(path) if is_stdin(path) => Box::new(io::stdin().lock()),
        Some(path) => {
            let file =
                File::open(path).with_context(|| format!("cannot open: {}", path.display()))?;
            Box::new(BufReader::new(file))
        }
    };

    // Determine what lines to output
    // Exclusion needs the whole input (the "-N" form counts back from the
    // end), so this path buffers everything; without --exclude the
    // streaming selection below is used instead.
    let lines: Vec<(usize, String)> = if let Some(ref exclude_spec) = args.exclude {
        let all: Vec<(usize, String)> = reader
            .lines()
            .enumerate()
            .map(|(i, l)| Ok((i + 1, l?)))
            .collect::<Result<Vec<_>, io::Error>>()
            .context("error reading file")?;
        let total = all.len();
        // Same selection semantics as the streaming path, then drop excluded.
        let selected: Vec<(usize, String)> = if let Some(ref spec) = args.lines {
            let (start, end) = parse_line_range(spec)?;
            match (start, end) {
                (None, Some(n)) => {
                    let skip = total.saturating_sub(n);
                    all.into_iter().skip(skip).collect()
                }
                (Some(start), Some(end)) => all
                    .into_iter()
                    .filter(|(num, _)| *num >= start && *num <= end)
                    .collect(),
                (Some(start), None) => all.into_iter().filter(|(num, _)| *num >= start).collect(),
                (None, None) => unreachable!(),
            }
        } else {
            all.into_iter().skip(args.offset).collect()
        };
        let ranges = parse_exclude_ranges(exclude_spec)?;
        selected
            .into_iter()
            .filter(|(num, _)| !is_excluded(*num, total, &ranges))
            .collect()
    } else if let Some(ref spec) = args.lines {
        let (start, end) = parse_line_range(spec)?;
        match (start, end) {
            (None, Some(n)) => {
                // Last n lines: read all, then take last n
                let all: Vec<(usize, String)> = reader
                    .lines()
                    .enumerate()
                    .map(|(i, l)| Ok((i + 1, l?)))
                    .collect::<Result<Vec<_>, io::Error>>()
                    .context("error reading file")?;
                let skip = all.len().saturating_sub(n);
                all.into_iter().skip(skip).collect()
            }
            (Some(start), Some(end)) => {
                // Specific range [start, end] 1-based inclusive
                reader
                    .lines()
                    .enumerate()
                    .filter_map(|(i, l)| {
                        let line_num = i + 1;
                        if line_num >= start && line_num <= end {
                            Some(l.map(|s| (line_num, s)))
                        } else if line_num > end {
                            None // Could break but filter_map doesn't support that
                        } else {
                            // Skip lines before start, but still consume them
                            let _ = l; // consume the Result
                            None
                        }
                    })
                    .collect::<Result<Vec<_>, io::Error>>()
                    .context("error reading file")?
            }
            (Some(start), None) => {
                // From start to end of file
                reader
                    .lines()
                    .enumerate()
                    .filter_map(|(i, l)| {
                        let line_num = i + 1;
                        if line_num >= start {
                            Some(l.map(|s| (line_num, s)))
                        } else {
                            let _ = l;
                            None
                        }
                    })
                    .collect::<Result<Vec<_>, io::Error>>()
                    .context("error reading file")?
            }
            (None, None) => unreachable!(),
        }
    } else {
        // No range: read all lines with offset
        reader
            .lines()
            .enumerate()
            .skip(args.offset)
            .map(|(i, l)| Ok((i + 1, l?)))
            .collect::<Result<Vec<_>, io::Error>>()
            .context("error reading file")?
    };

    if lines.is_empty() {
        return Ok(Outcome::NotFound);
    }

    let max_line_num = lines.last().map(|(n, _)| *n).unwrap_or(1);
    let width = line_number_width(max_line_num);

    let stdout = io::stdout();
    let handle = stdout.lock();
    // A caller who never passed `--limit` can still be truncated by the
    // built-in cap, so that case announces itself on stderr; an explicit
    // `--limit N` is exactly what was asked for and truncates silently.
    let limit = args
        .limit
        .map(Limit::Explicit)
        .unwrap_or(Limit::Default(DEFAULT_LIMIT));
    let mut writer = BoundedWriter::new(handle, limit);

    for (line_num, content) in &lines {
        let output = if args.no_line_numbers {
            content.to_string()
        } else {
            format!("{}{}", format_line_number(*line_num, width), content)
        };
        if !writer.write_line(&output)? {
            break;
        }
    }

    writer.flush()?;
    Ok(Outcome::Found)
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    /// Wrapper so we can drive `ReadArgs` through clap parsing in tests.
    #[derive(Parser)]
    struct ReadCli {
        #[command(flatten)]
        args: ReadArgs,
    }

    #[test]
    fn test_clap_parses_negative_n() {
        let cli = ReadCli::try_parse_from(["read", "file.txt", "-n", "-20"]).unwrap();
        assert_eq!(cli.args.lines.as_deref(), Some("-20"));
        let (s, e) = parse_line_range(cli.args.lines.as_deref().unwrap()).unwrap();
        assert_eq!(s, None);
        assert_eq!(e, Some(20));
    }

    #[test]
    fn test_clap_omitted_file_means_stdin() {
        let cli = ReadCli::try_parse_from(["read"]).unwrap();
        assert_eq!(cli.args.file, None);
        let cli = ReadCli::try_parse_from(["read", "-"]).unwrap();
        assert_eq!(cli.args.file, Some(PathBuf::from("-")));
        let cli = ReadCli::try_parse_from(["read", "file.txt"]).unwrap();
        assert_eq!(cli.args.file, Some(PathBuf::from("file.txt")));
    }

    #[test]
    fn test_clap_parses_range_forms() {
        // Make sure allow_hyphen_values didn't regress the other forms.
        let cli = ReadCli::try_parse_from(["read", "file.txt", "-n", "1-50"]).unwrap();
        assert_eq!(cli.args.lines.as_deref(), Some("1-50"));
        let cli = ReadCli::try_parse_from(["read", "file.txt", "-n", "100-"]).unwrap();
        assert_eq!(cli.args.lines.as_deref(), Some("100-"));
    }

    #[test]
    fn test_parse_range_full() {
        let (s, e) = parse_line_range("1-50").unwrap();
        assert_eq!(s, Some(1));
        assert_eq!(e, Some(50));
    }

    #[test]
    fn test_parse_range_from() {
        let (s, e) = parse_line_range("100-").unwrap();
        assert_eq!(s, Some(100));
        assert_eq!(e, None);
    }

    #[test]
    fn test_parse_range_last_n() {
        let (s, e) = parse_line_range("-20").unwrap();
        assert_eq!(s, None);
        assert_eq!(e, Some(20));
    }

    #[test]
    fn test_parse_range_single() {
        let (s, e) = parse_line_range("42").unwrap();
        assert_eq!(s, Some(42));
        assert_eq!(e, Some(42));
    }

    #[test]
    fn test_read_file() {
        use std::io::Write;
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("test.txt");
        {
            let mut f = File::create(&file_path).unwrap();
            for i in 1..=10 {
                writeln!(f, "line {}", i).unwrap();
            }
        }

        let args = ReadArgs {
            file: Some(file_path),
            lines: Some("3-5".to_string()),
            exclude: None,
            no_line_numbers: true,
            limit: None,
            offset: 0,
        };
        let exit = run(&args).unwrap();
        assert_eq!(exit, Outcome::Found);
    }

    #[test]
    fn test_read_last_n() {
        use std::io::Write;
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("test.txt");
        {
            let mut f = File::create(&file_path).unwrap();
            for i in 1..=10 {
                writeln!(f, "line {}", i).unwrap();
            }
        }

        let args = ReadArgs {
            file: Some(file_path),
            lines: Some("-3".to_string()),
            exclude: None,
            no_line_numbers: true,
            limit: None,
            offset: 0,
        };
        let exit = run(&args).unwrap();
        assert_eq!(exit, Outcome::Found);
    }

    #[test]
    fn test_clap_parses_exclude_forms() {
        let cli = ReadCli::try_parse_from(["read", "f.txt", "--exclude", "1-10,90-100"]).unwrap();
        assert_eq!(cli.args.exclude.as_deref(), Some("1-10,90-100"));
        // A leading hyphen must still parse as the flag's value.
        let cli = ReadCli::try_parse_from(["read", "f.txt", "--exclude", "-5"]).unwrap();
        assert_eq!(cli.args.exclude.as_deref(), Some("-5"));
    }

    #[test]
    fn test_parse_exclude_ranges() {
        let ranges = parse_exclude_ranges("1-10,90-100").unwrap();
        assert_eq!(ranges, vec![(Some(1), Some(10)), (Some(90), Some(100))]);
        let ranges = parse_exclude_ranges("42").unwrap();
        assert_eq!(ranges, vec![(Some(42), Some(42))]);
        let ranges = parse_exclude_ranges("100-,-5").unwrap();
        assert_eq!(ranges, vec![(Some(100), None), (None, Some(5))]);
        assert!(parse_exclude_ranges("1-10,abc").is_err());
        assert!(parse_exclude_ranges("").is_err());
    }

    #[test]
    fn test_is_excluded() {
        let ranges = parse_exclude_ranges("3-5,8-").unwrap();
        assert!(!is_excluded(1, 10, &ranges));
        assert!(is_excluded(3, 10, &ranges));
        assert!(is_excluded(5, 10, &ranges));
        assert!(!is_excluded(7, 10, &ranges));
        assert!(is_excluded(8, 10, &ranges));
        assert!(is_excluded(10, 10, &ranges));
    }

    #[test]
    fn test_is_excluded_last_n() {
        // "-3" of 10 lines drops 8, 9, 10 however many lines came before.
        let ranges = parse_exclude_ranges("-3").unwrap();
        assert!(!is_excluded(7, 10, &ranges));
        assert!(is_excluded(8, 10, &ranges));
        assert!(is_excluded(10, 10, &ranges));
        // Asking for more lines than exist drops everything.
        assert!(is_excluded(1, 3, &ranges));
    }

    #[test]
    fn test_read_exclude() {
        use std::io::Write;
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("test.txt");
        {
            let mut f = File::create(&file_path).unwrap();
            for i in 1..=10 {
                writeln!(f, "line {}", i).unwrap();
            }
        }

        let args = ReadArgs {
            file: Some(file_path.clone()),
            lines: None,
            exclude: Some("3-5".to_string()),
            no_line_numbers: true,
            limit: None,
            offset: 0,
        };
        assert_eq!(run(&args).unwrap(), Outcome::Found);

        // Excluding everything is an empty result, not an error.
        let args = ReadArgs {
            file: Some(file_path),
            lines: None,
            exclude: Some("1-".to_string()),
            no_line_numbers: true,
            limit: None,
            offset: 0,
        };
        assert_eq!(run(&args).unwrap(), Outcome::NotFound);
    }

    #[test]
    fn test_read_exclude_composes_with_lines() {
        use std::io::Write;
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("test.txt");
        {
            let mut f = File::create(&file_path).unwrap();
            for i in 1..=10 {
                writeln!(f, "line {}", i).unwrap();
            }
        }

        // -n selects 1-6, --exclude drops 2-3 of the original numbering.
        let args = ReadArgs {
            file: Some(file_path),
            lines: Some("1-6".to_string()),
            exclude: Some("2-3".to_string()),
            no_line_numbers: true,
            limit: None,
            offset: 0,
        };
        assert_eq!(run(&args).unwrap(), Outcome::Found);
    }
}
