use crate::output::Outcome;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::Args;

use super::read::{ReadArgs, run as read_run};

#[derive(Args)]
#[command(
    about = "Show the last N lines (or bytes) of a file",
    long_about = "Show the last N lines of a file (default 10).\n\n\
        An ergonomic shorthand for `sak fs read <file> -n -N`: line numbers are \
        on by default and reflect the lines' real positions in the file (disable \
        with --no-line-numbers). Pass --bytes to emit the last N bytes raw \
        instead of whole lines (the byte output is written verbatim, with no \
        line numbers).",
    after_help = "\
Examples:
  sak fs tail logfile.txt                  Last 10 lines
  sak fs tail logfile.txt 50               Last 50 lines
  sak fs tail --no-line-numbers file.txt   Last 10 lines, no line numbers
  sak fs tail --bytes 256 capture.bin      Last 256 bytes (raw)"
)]
pub struct TailArgs {
    /// Path to the file to read
    pub file: PathBuf,

    /// Number of lines to show (default 10)
    #[arg(value_name = "N", conflicts_with = "bytes")]
    pub lines: Option<usize>,

    /// Show the last N bytes instead of lines (raw output)
    #[arg(long, value_name = "N")]
    pub bytes: Option<usize>,

    /// Omit line numbers from output
    #[arg(long = "no-line-numbers")]
    pub no_line_numbers: bool,
}

pub fn run(args: &TailArgs) -> Result<Outcome> {
    if let Some(n) = args.bytes {
        return tail_bytes(&args.file, n);
    }
    let n = args.lines.unwrap_or(10);
    // Delegate the line path to `read`'s last-N handling so tail stays a thin,
    // consistent wrapper (and line numbers reflect real file positions).
    let read_args = ReadArgs {
        file: args.file.clone(),
        lines: Some(format!("-{n}")),
        no_line_numbers: args.no_line_numbers,
        limit: n,
        offset: 0,
    };
    read_run(&read_args)
}

/// Write the last `n` bytes of `file` raw to stdout (byte-faithful).
fn tail_bytes(file: &PathBuf, n: usize) -> Result<Outcome> {
    let mut f =
        std::fs::File::open(file).with_context(|| format!("cannot open: {}", file.display()))?;
    let len = f
        .metadata()
        .with_context(|| format!("cannot stat: {}", file.display()))?
        .len();
    let n = (n as u64).min(len);
    if n == 0 {
        return Ok(Outcome::NotFound);
    }
    f.seek(SeekFrom::Start(len - n))
        .with_context(|| format!("error seeking: {}", file.display()))?;
    let mut buf = Vec::with_capacity(n.min(64 * 1024) as usize);
    f.read_to_end(&mut buf)
        .with_context(|| format!("error reading: {}", file.display()))?;
    let stdout = io::stdout();
    stdout.lock().write_all(&buf)?;
    Ok(Outcome::Found)
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
    fn tail_lines_default() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("f.txt");
        write_lines(&p, 50);
        let args = TailArgs {
            file: p,
            lines: None,
            bytes: None,
            no_line_numbers: true,
        };
        assert_eq!(run(&args).unwrap(), Outcome::Found);
    }

    #[test]
    fn tail_bytes_mode() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("f.bin");
        std::fs::write(&p, b"abcdefghij").unwrap();
        let args = TailArgs {
            file: p,
            lines: None,
            bytes: Some(3),
            no_line_numbers: false,
        };
        assert_eq!(run(&args).unwrap(), Outcome::Found);
    }

    #[test]
    fn tail_bytes_clamps_to_file_len() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("f.bin");
        std::fs::write(&p, b"abc").unwrap();
        let args = TailArgs {
            file: p,
            lines: None,
            bytes: Some(1000),
            no_line_numbers: false,
        };
        assert_eq!(run(&args).unwrap(), Outcome::Found);
    }

    #[test]
    fn tail_empty_file_exit_1() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("empty.txt");
        std::fs::write(&p, b"").unwrap();
        let args = TailArgs {
            file: p,
            lines: None,
            bytes: Some(10),
            no_line_numbers: false,
        };
        assert_eq!(run(&args).unwrap(), Outcome::NotFound);
    }
}
