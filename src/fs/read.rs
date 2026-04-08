use std::fs::File;
use std::io::{self, BufRead, BufReader};
use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{Context, Result};
use clap::Args;

use crate::output::{BoundedWriter, format_line_number, line_number_width};

#[derive(Args)]
#[command(
    about = "Read file contents with line numbers",
    long_about = "Read file contents with line numbers.\n\n\
        Displays the contents of a file with line numbers for easy reference. \
        Supports reading specific line ranges and limiting output length.",
    after_help = "\
Examples:
  sak fs read src/main.rs                  Read entire file (up to 2000 lines)
  sak fs read src/main.rs -n 1-50          Read lines 1 through 50
  sak fs read src/main.rs -n 100-          Read from line 100 to end
  sak fs read src/main.rs -n -20           Read last 20 lines
  sak fs read src/main.rs --offset 10 --limit 5   Skip 10 lines, show 5"
)]
pub struct ReadArgs {
    /// Path to the file to read
    pub file: PathBuf,

    /// Line range to read (e.g., "1-50", "100-", "-20")
    #[arg(short = 'n', long = "lines")]
    pub lines: Option<String>,

    /// Omit line numbers from output
    #[arg(long = "no-line-numbers")]
    pub no_line_numbers: bool,

    /// Maximum number of lines to output
    #[arg(long, default_value = "2000")]
    pub limit: usize,

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

pub fn run(args: &ReadArgs) -> Result<ExitCode> {
    let file =
        File::open(&args.file).with_context(|| format!("cannot open: {}", args.file.display()))?;
    let reader = BufReader::new(file);

    // Determine what lines to output
    let lines: Vec<(usize, String)> = if let Some(ref spec) = args.lines {
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
        return Ok(ExitCode::from(1));
    }

    let max_line_num = lines.last().map(|(n, _)| *n).unwrap_or(1);
    let width = line_number_width(max_line_num);

    let stdout = io::stdout();
    let handle = stdout.lock();
    let mut writer = BoundedWriter::new(handle, Some(args.limit));

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
    Ok(ExitCode::SUCCESS)
}

#[cfg(test)]
mod tests {
    use super::*;

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
            file: file_path,
            lines: Some("3-5".to_string()),
            no_line_numbers: true,
            limit: 2000,
            offset: 0,
        };
        let exit = run(&args).unwrap();
        assert_eq!(exit, ExitCode::SUCCESS);
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
            file: file_path,
            lines: Some("-3".to_string()),
            no_line_numbers: true,
            limit: 2000,
            offset: 0,
        };
        let exit = run(&args).unwrap();
        assert_eq!(exit, ExitCode::SUCCESS);
    }
}
