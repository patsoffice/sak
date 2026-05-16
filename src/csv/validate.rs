use std::io::{self, BufReader, Read};
use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{Context, Result};
use clap::Args;

use crate::output::BoundedWriter;

use super::headers::parse_delimiter;

#[derive(Args)]
#[command(
    about = "Check CSV structure and report parse errors",
    long_about = "Check CSV files for structural problems and report errors.\n\n\
        Detects: invalid UTF-8, unclosed quotes, malformed records. With \
        --strict, also rejects rows whose column count differs from the \
        header. Reads from stdin when no files are given. Exits 0 if every \
        input parsed cleanly, exits 1 if any error was reported. Errors go \
        to stderr as `<source>:<line>: <message>` — stdout is reserved for \
        the per-file `<source>: valid` summary unless --quiet is set.",
    after_help = "\
Examples:
  sak csv validate data.csv                     Check a single file
  sak csv validate *.csv                        Check every CSV in cwd
  sak csv validate --strict data.csv            Also reject mismatched col counts
  sak csv validate --quiet data.csv             Exit code only
  cat data.csv | sak csv validate               Validate piped input
  sak csv validate -d $'\\t' data.tsv            Tab-delimited input"
)]
pub struct ValidateArgs {
    /// Input CSV files (reads stdin if omitted)
    pub files: Vec<PathBuf>,

    /// Field delimiter (must be a single byte; default: ',')
    #[arg(short = 'd', long = "delimiter", default_value = ",")]
    pub delimiter: String,

    /// Reject rows whose column count differs from the header
    #[arg(long)]
    pub strict: bool,

    /// Exit code only, no output
    #[arg(short, long)]
    pub quiet: bool,

    /// Maximum number of output lines
    #[arg(long)]
    pub limit: Option<usize>,
}

/// Validate one source and return any error messages collected (each
/// formatted with the `<source>:<line>: ...` prefix). The csv reader stops
/// at the first error, so the returned vector has at most one entry today;
/// it's a Vec to leave room for future per-row reporting without changing
/// the call sites.
fn validate_one<R: Read>(source: &str, reader: R, delimiter: u8, strict: bool) -> Vec<String> {
    let mut rdr = ::csv::ReaderBuilder::new()
        .delimiter(delimiter)
        .has_headers(true)
        .flexible(!strict)
        .from_reader(reader);

    let mut errors = Vec::new();

    // Headers are parsed lazily on the first iteration, but a malformed
    // header is reported via the records() iterator just like a malformed
    // body row, so we don't need to call .headers() explicitly.
    for rec in rdr.records() {
        if let Err(e) = rec {
            errors.push(format_csv_error(source, &e));
            // The reader is poisoned after a parse error — stop here rather
            // than yielding a flood of cascading failures from one bad row.
            break;
        }
    }

    errors
}

fn format_csv_error(source: &str, err: &::csv::Error) -> String {
    if let Some(pos) = err.position() {
        format!("{}:{}: {}", source, pos.line(), err)
    } else {
        format!("{}: {}", source, err)
    }
}

pub fn run(args: &ValidateArgs) -> Result<ExitCode> {
    let delim = parse_delimiter(&args.delimiter)?;
    let stdout = io::stdout();
    let handle = stdout.lock();
    let mut writer = BoundedWriter::new(handle, args.limit);

    let mut any_invalid = false;

    if args.files.is_empty() {
        let stdin = io::stdin();
        let reader = stdin.lock();
        let errors = validate_one("<stdin>", reader, delim, args.strict);
        if errors.is_empty() {
            if !args.quiet {
                writer.write_line("<stdin>: valid")?;
            }
        } else {
            any_invalid = true;
            if !args.quiet {
                for e in errors {
                    eprintln!("{}", e);
                }
            }
        }
    } else {
        for path in &args.files {
            let name = path.display().to_string();
            let file = match std::fs::File::open(path) {
                Ok(f) => f,
                Err(e) => {
                    any_invalid = true;
                    if !args.quiet {
                        eprintln!("{}: cannot read: {}", name, e);
                    }
                    continue;
                }
            };
            let errors = validate_one(&name, BufReader::new(file), delim, args.strict);
            if errors.is_empty() {
                if !args.quiet && !writer.write_line(&format!("{}: valid", name))? {
                    break;
                }
            } else {
                any_invalid = true;
                if !args.quiet {
                    for e in errors {
                        eprintln!("{}", e);
                    }
                }
            }
        }
    }

    writer.flush().context("flushing stdout")?;
    if any_invalid {
        Ok(ExitCode::from(1))
    } else {
        Ok(ExitCode::SUCCESS)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write(tmp: &std::path::Path, name: &str, body: &str) -> PathBuf {
        let p = tmp.join(name);
        let mut f = std::fs::File::create(&p).unwrap();
        f.write_all(body.as_bytes()).unwrap();
        p
    }

    #[test]
    fn valid_file() {
        let dir = tempfile::tempdir().unwrap();
        let p = write(dir.path(), "a.csv", "name,age\nalice,30\nbob,25\n");
        let args = ValidateArgs {
            files: vec![p],
            delimiter: ",".to_string(),
            strict: true,
            quiet: true,
            limit: None,
        };
        assert_eq!(run(&args).unwrap(), ExitCode::SUCCESS);
    }

    #[test]
    fn strict_catches_mismatched_columns() {
        let dir = tempfile::tempdir().unwrap();
        let p = write(dir.path(), "bad.csv", "a,b,c\n1,2\n");
        let errors = validate_one(
            "bad.csv",
            BufReader::new(std::fs::File::open(&p).unwrap()),
            b',',
            true,
        );
        assert!(!errors.is_empty(), "expected mismatched-column error");
        assert!(errors[0].starts_with("bad.csv:"), "got {:?}", errors[0]);
    }

    #[test]
    fn flexible_allows_mismatched_columns() {
        let dir = tempfile::tempdir().unwrap();
        let p = write(dir.path(), "ragged.csv", "a,b,c\n1,2\n");
        let errors = validate_one(
            "ragged.csv",
            BufReader::new(std::fs::File::open(&p).unwrap()),
            b',',
            false,
        );
        assert!(errors.is_empty(), "expected no errors, got {:?}", errors);
    }

    #[test]
    fn unclosed_quote_is_reported() {
        // The csv crate stops on the unclosed quote and reports an UnequalLengths-
        // style error; either way the position should pin a line number.
        let dir = tempfile::tempdir().unwrap();
        let p = write(dir.path(), "quote.csv", "a,b\n\"oops,2\nfine,3\n");
        let errors = validate_one(
            "quote.csv",
            BufReader::new(std::fs::File::open(&p).unwrap()),
            b',',
            true,
        );
        assert!(!errors.is_empty());
        assert!(errors[0].contains("quote.csv:"));
    }

    #[test]
    fn missing_file_exits_nonzero() {
        let args = ValidateArgs {
            files: vec![PathBuf::from("/nonexistent/path/should-not-exist.csv")],
            delimiter: ",".to_string(),
            strict: false,
            quiet: true,
            limit: None,
        };
        assert_eq!(run(&args).unwrap(), ExitCode::from(1));
    }
}
