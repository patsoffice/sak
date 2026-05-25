use std::io::{self, Read, Write};
use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{Context, Result};
use clap::Args;

use super::read::{ReadArgs, run as read_run};

#[derive(Args)]
#[command(
    about = "Show the first N lines (or bytes) of a file",
    long_about = "Show the first N lines of a file (default 10).\n\n\
        An ergonomic shorthand for `sak fs read <file> -n 1-N`: line numbers are \
        on by default (disable with --no-line-numbers). Pass --bytes to emit the \
        first N bytes raw instead of whole lines — useful for peeking at binary \
        headers (the byte output is written verbatim, with no line numbers).",
    after_help = "\
Examples:
  sak fs head src/main.rs                  First 10 lines
  sak fs head src/main.rs 25               First 25 lines
  sak fs head --no-line-numbers file.txt   First 10 lines, no line numbers
  sak fs head --bytes 64 image.png         First 64 bytes (raw)"
)]
pub struct HeadArgs {
    /// Path to the file to read
    pub file: PathBuf,

    /// Number of lines to show (default 10)
    #[arg(value_name = "N", conflicts_with = "bytes")]
    pub lines: Option<usize>,

    /// Show the first N bytes instead of lines (raw output)
    #[arg(long, value_name = "N")]
    pub bytes: Option<usize>,

    /// Omit line numbers from output
    #[arg(long = "no-line-numbers")]
    pub no_line_numbers: bool,
}

pub fn run(args: &HeadArgs) -> Result<ExitCode> {
    if let Some(n) = args.bytes {
        return head_bytes(&args.file, n);
    }
    let n = args.lines.unwrap_or(10);
    // Delegate the line path to `read` so head stays a thin, consistent wrapper.
    let read_args = ReadArgs {
        file: args.file.clone(),
        lines: Some(format!("1-{n}")),
        no_line_numbers: args.no_line_numbers,
        limit: n,
        offset: 0,
    };
    read_run(&read_args)
}

/// Write the first `n` bytes of `file` raw to stdout (byte-faithful).
fn head_bytes(file: &PathBuf, n: usize) -> Result<ExitCode> {
    let f =
        std::fs::File::open(file).with_context(|| format!("cannot open: {}", file.display()))?;
    let mut buf = Vec::with_capacity(n.min(64 * 1024));
    f.take(n as u64)
        .read_to_end(&mut buf)
        .with_context(|| format!("error reading: {}", file.display()))?;
    if buf.is_empty() {
        return Ok(ExitCode::from(1));
    }
    let stdout = io::stdout();
    stdout.lock().write_all(&buf)?;
    Ok(ExitCode::SUCCESS)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_lines(path: &std::path::Path, n: usize) {
        let mut f = std::fs::File::create(path).unwrap();
        for i in 1..=n {
            writeln!(f, "line {i}").unwrap();
        }
    }

    #[test]
    fn head_lines_default() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("f.txt");
        write_lines(&p, 50);
        let args = HeadArgs {
            file: p,
            lines: None,
            bytes: None,
            no_line_numbers: true,
        };
        assert_eq!(run(&args).unwrap(), ExitCode::SUCCESS);
    }

    #[test]
    fn head_bytes_mode() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("f.bin");
        std::fs::write(&p, b"abcdefghij").unwrap();
        let args = HeadArgs {
            file: p,
            lines: None,
            bytes: Some(4),
            no_line_numbers: false,
        };
        assert_eq!(run(&args).unwrap(), ExitCode::SUCCESS);
    }

    #[test]
    fn head_empty_file_exit_1() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("empty.txt");
        std::fs::write(&p, b"").unwrap();
        let args = HeadArgs {
            file: p,
            lines: None,
            bytes: None,
            no_line_numbers: true,
        };
        assert_eq!(run(&args).unwrap(), ExitCode::from(1));
    }
}
