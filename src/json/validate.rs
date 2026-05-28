use crate::output::Outcome;
use std::io::{self, Read};
use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::Args;
use serde_json::Value;

use crate::output::BoundedWriter;

#[derive(Args)]
#[command(
    about = "Check whether files contain valid JSON",
    long_about = "Check whether files contain valid JSON and report parse errors.\n\n\
        Reads from stdin if no files are given, or for a file argument of `-`. \
        Exits 0 if all inputs are valid, \
        exits 1 if any input is invalid. Errors are reported to stderr as \
        `filename:line:column: message`.\n\n\
        With `--lines`, each non-blank line of the input is validated independently \
        as NDJSON. Errors are reported per-line as `filename:lineno: message` and \
        validation continues across invalid lines. Blank / whitespace-only lines \
        are skipped.",
    after_help = "\
Examples:
  sak json validate config.json              Validate a single file
  sak json validate *.json                   Validate multiple files
  echo '{\"a\":1}' | sak json validate       Validate piped input
  sak json validate --quiet config.json      Exit code only, no output
  sak json validate --lines events.ndjson    Validate each NDJSON record"
)]
pub struct ValidateArgs {
    /// Input files (reads stdin if omitted or given as "-")
    pub files: Vec<PathBuf>,

    /// Exit code only, no output
    #[arg(short, long)]
    pub quiet: bool,

    /// Strict parsing (default; reserved for future lenient mode)
    #[arg(long)]
    pub strict: bool,

    /// Parse input as NDJSON (one JSON value per line); errors reported per line
    #[arg(long)]
    pub lines: bool,

    /// Maximum number of output lines
    #[arg(long)]
    pub limit: Option<usize>,
}

fn validate_one(name: &str, content: &str) -> std::result::Result<(), String> {
    match serde_json::from_str::<Value>(content) {
        Ok(_) => Ok(()),
        Err(e) => Err(format!("{}:{}:{}: {}", name, e.line(), e.column(), e)),
    }
}

/// Validate `content` as NDJSON. Returns (total_non_blank_lines, error_messages).
/// Each error message is prefixed with `name:lineno:`.
fn validate_lines(name: &str, content: &str) -> (usize, Vec<String>) {
    let mut total = 0;
    let mut errs = Vec::new();
    for (idx, line) in content.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        total += 1;
        let lineno = idx + 1;
        if let Err(e) = serde_json::from_str::<Value>(line) {
            errs.push(format!("{}:{}: {}", name, lineno, e));
        }
    }
    (total, errs)
}

pub fn run(args: &ValidateArgs) -> Result<Outcome> {
    let _ = args.strict; // reserved
    let stdout = io::stdout();
    let handle = stdout.lock();
    let mut writer = BoundedWriter::new(handle, args.limit);

    let mut any_invalid = false;

    let mut sources: Vec<(String, String)> = Vec::new();
    if args.files.is_empty() {
        let mut s = String::new();
        io::stdin()
            .read_to_string(&mut s)
            .context("error reading stdin")?;
        sources.push(("<stdin>".to_string(), s));
    } else {
        for path in &args.files {
            if crate::json::is_stdin(path) {
                let mut s = String::new();
                io::stdin()
                    .read_to_string(&mut s)
                    .context("error reading stdin")?;
                sources.push(("<stdin>".to_string(), s));
                continue;
            }
            let name = path.display().to_string();
            match std::fs::read_to_string(path) {
                Ok(s) => sources.push((name, s)),
                Err(e) => {
                    any_invalid = true;
                    if !args.quiet {
                        eprintln!("{}: cannot read: {}", name, e);
                    }
                }
            }
        }
    }

    for (name, content) in &sources {
        if args.lines {
            let (total, errs) = validate_lines(name, content);
            if !errs.is_empty() {
                any_invalid = true;
                if !args.quiet {
                    for msg in &errs {
                        eprintln!("{}", msg);
                    }
                }
            }
            if !args.quiet && errs.is_empty() {
                let line = format!("{}: valid ({} records)", name, total);
                if !writer.write_line(&line)? {
                    break;
                }
            }
        } else {
            match validate_one(name, content) {
                Ok(()) => {
                    if !args.quiet && !writer.write_line(&format!("{}: valid", name))? {
                        break;
                    }
                }
                Err(msg) => {
                    any_invalid = true;
                    if !args.quiet {
                        eprintln!("{}", msg);
                    }
                }
            }
        }
    }

    writer.flush()?;
    if any_invalid {
        Ok(Outcome::NotFound)
    } else {
        Ok(Outcome::Found)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn valid_file() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.json");
        let mut f = std::fs::File::create(&p).unwrap();
        writeln!(f, r#"{{"a": 1}}"#).unwrap();
        let args = ValidateArgs {
            files: vec![p],
            quiet: true,
            strict: false,
            lines: false,
            limit: None,
        };
        let exit = run(&args).unwrap();
        assert_eq!(exit, Outcome::Found);
    }

    #[test]
    fn invalid_file() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("bad.json");
        let mut f = std::fs::File::create(&p).unwrap();
        writeln!(f, "{{not json").unwrap();
        let args = ValidateArgs {
            files: vec![p],
            quiet: true,
            strict: false,
            lines: false,
            limit: None,
        };
        let exit = run(&args).unwrap();
        assert_eq!(exit, Outcome::NotFound);
    }

    #[test]
    fn validate_one_reports_position() {
        let err = validate_one("x.json", "{\n  bad").unwrap_err();
        assert!(err.starts_with("x.json:"));
    }

    #[test]
    fn validate_lines_counts_records_and_skips_blanks() {
        let (total, errs) = validate_lines("f.ndjson", "{\"a\":1}\n\n{\"b\":2}\n   \n{\"c\":3}\n");
        assert_eq!(total, 3);
        assert!(errs.is_empty());
    }

    #[test]
    fn validate_lines_reports_bad_records_with_lineno() {
        let (total, errs) = validate_lines("f.ndjson", "{\"a\":1}\nnot json\n{\"b\":2}\n");
        assert_eq!(total, 3);
        assert_eq!(errs.len(), 1);
        assert!(errs[0].starts_with("f.ndjson:2:"));
    }

    #[test]
    fn lines_mode_all_valid_succeeds() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.ndjson");
        let mut f = std::fs::File::create(&p).unwrap();
        f.write_all(b"{\"a\":1}\n{\"a\":2}\n").unwrap();
        let args = ValidateArgs {
            files: vec![p],
            quiet: true,
            strict: false,
            lines: true,
            limit: None,
        };
        assert_eq!(run(&args).unwrap(), Outcome::Found);
    }

    #[test]
    fn lines_mode_one_bad_record_fails() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("bad.ndjson");
        let mut f = std::fs::File::create(&p).unwrap();
        f.write_all(b"{\"a\":1}\nnope\n{\"a\":2}\n").unwrap();
        let args = ValidateArgs {
            files: vec![p],
            quiet: true,
            strict: false,
            lines: true,
            limit: None,
        };
        assert_eq!(run(&args).unwrap(), Outcome::NotFound);
    }
}
