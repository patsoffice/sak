use crate::output::Outcome;
use std::io::{self, Read};
use std::path::PathBuf;

use anyhow::Result;
use clap::Args;

use crate::output::BoundedWriter;

#[derive(Args)]
#[command(
    about = "Count lines, words, and bytes in files",
    long_about = "Count lines, words, and bytes in one or more files.\n\n\
        Prints `lines<TAB>words<TAB>bytes<TAB>filename` per file (matching `wc`'s \
        definitions: lines are newline characters, words are whitespace-separated \
        runs, bytes are the raw byte length). With more than one file, a `total` \
        row is appended. With no files, counts standard input and omits the \
        filename column.\n\n\
        Pass any of --lines/--words/--bytes to restrict the output to those \
        columns (in that fixed order); with none given, all three are shown.",
    after_help = "\
Examples:
  sak fs wc src/main.rs                    Lines, words, bytes of one file
  sak fs wc src/*.rs                        Per-file counts plus a total row
  sak fs wc --lines src/main.rs            Line count only
  cat file | sak fs wc                      Count standard input"
)]
pub struct WcArgs {
    /// Files to count (reads standard input when none are given)
    pub files: Vec<PathBuf>,

    /// Show the line count
    #[arg(short = 'l', long)]
    pub lines: bool,

    /// Show the word count
    #[arg(short = 'w', long)]
    pub words: bool,

    /// Show the byte count
    #[arg(short = 'c', long)]
    pub bytes: bool,

    /// Maximum number of lines to output
    #[arg(long)]
    pub limit: Option<usize>,
}

#[derive(Default, Clone, Copy)]
struct Counts {
    lines: usize,
    words: usize,
    bytes: usize,
}

impl Counts {
    fn add(&mut self, other: &Counts) {
        self.lines += other.lines;
        self.words += other.words;
        self.bytes += other.bytes;
    }
}

/// Count newlines, whitespace-separated words, and bytes in `data`.
fn count(data: &[u8]) -> Counts {
    let bytes = data.len();
    let lines = data.iter().filter(|&&b| b == b'\n').count();
    // Words: runs of non-whitespace, matching `wc`. Operate on the lossy string
    // so multibyte UTF-8 whitespace doesn't split a word mid-codepoint.
    let words = String::from_utf8_lossy(data).split_whitespace().count();
    Counts {
        lines,
        words,
        bytes,
    }
}

/// Render a counts row honoring the selected columns. When `name` is `None`
/// (stdin) the filename column is omitted entirely.
fn row(c: &Counts, name: Option<&str>, sel: (bool, bool, bool)) -> String {
    let (l, w, b) = sel;
    let mut cols: Vec<String> = Vec::with_capacity(4);
    if l {
        cols.push(c.lines.to_string());
    }
    if w {
        cols.push(c.words.to_string());
    }
    if b {
        cols.push(c.bytes.to_string());
    }
    if let Some(n) = name {
        cols.push(n.to_string());
    }
    cols.join("\t")
}

pub fn run(args: &WcArgs) -> Result<Outcome> {
    // No explicit selection means show all three columns.
    let sel = if !(args.lines || args.words || args.bytes) {
        (true, true, true)
    } else {
        (args.lines, args.words, args.bytes)
    };

    let stdout = io::stdout();
    let mut writer = BoundedWriter::new(stdout.lock(), args.limit);

    if args.files.is_empty() {
        let mut data = Vec::new();
        io::stdin().read_to_end(&mut data)?;
        let c = count(&data);
        writer.write_line(&row(&c, None, sel))?;
        writer.flush()?;
        return Ok(Outcome::Found);
    }

    let mut total = Counts::default();
    let mut any_error = false;
    for file in &args.files {
        let data = match std::fs::read(file) {
            Ok(d) => d,
            Err(e) => {
                eprintln!("sak: error: cannot read {}: {e}", file.display());
                any_error = true;
                continue;
            }
        };
        let c = count(&data);
        total.add(&c);
        if !writer.write_line(&row(&c, Some(&file.display().to_string()), sel))? {
            break;
        }
    }

    if args.files.len() > 1 {
        writer.write_line(&row(&total, Some("total"), sel))?;
    }
    writer.flush()?;

    if any_error {
        Ok(Outcome::Partial)
    } else {
        Ok(Outcome::Found)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_lines_words_bytes() {
        let c = count(b"hello world\nfoo bar baz\n");
        assert_eq!(c.lines, 2);
        assert_eq!(c.words, 5);
        assert_eq!(c.bytes, 24);
    }

    #[test]
    fn no_trailing_newline_counts_zero_lines() {
        // `wc` counts newline characters, so a single line with no terminating
        // newline has a line count of 0 but a word/byte count.
        let c = count(b"oneword");
        assert_eq!(c.lines, 0);
        assert_eq!(c.words, 1);
        assert_eq!(c.bytes, 7);
    }

    #[test]
    fn row_selects_columns() {
        let c = Counts {
            lines: 3,
            words: 9,
            bytes: 40,
        };
        assert_eq!(
            row(&c, Some("f.txt"), (true, true, true)),
            "3\t9\t40\tf.txt"
        );
        assert_eq!(row(&c, Some("f.txt"), (true, false, false)), "3\tf.txt");
        assert_eq!(row(&c, None, (false, false, true)), "40");
    }

    #[test]
    fn run_multiple_files_succeeds() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "a b c\n").unwrap();
        std::fs::write(dir.path().join("b.txt"), "d e\nf\n").unwrap();
        let args = WcArgs {
            files: vec![dir.path().join("a.txt"), dir.path().join("b.txt")],
            lines: false,
            words: false,
            bytes: false,
            limit: None,
        };
        assert_eq!(run(&args).unwrap(), Outcome::Found);
    }

    #[test]
    fn run_missing_file_is_exit_2() {
        let args = WcArgs {
            files: vec![PathBuf::from("/no/such/file/xyz")],
            lines: false,
            words: false,
            bytes: false,
            limit: None,
        };
        assert_eq!(run(&args).unwrap(), Outcome::Partial);
    }
}
