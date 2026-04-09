use std::io::{self, Read};
use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{Context, Result};
use clap::Args;
use serde_json::Value;

use crate::output::BoundedWriter;

#[derive(Args)]
#[command(
    about = "Check whether files contain valid JSON",
    long_about = "Check whether files contain valid JSON and report parse errors.\n\n\
        Reads from stdin if no files are given. Exits 0 if all inputs are valid, \
        exits 1 if any input is invalid. Errors are reported to stderr as \
        `filename:line:column: message`.",
    after_help = "\
Examples:
  sak json validate config.json              Validate a single file
  sak json validate *.json                   Validate multiple files
  echo '{\"a\":1}' | sak json validate       Validate piped input
  sak json validate --quiet config.json      Exit code only, no output"
)]
pub struct ValidateArgs {
    /// Input files (reads stdin if omitted)
    pub files: Vec<PathBuf>,

    /// Exit code only, no output
    #[arg(short, long)]
    pub quiet: bool,

    /// Strict parsing (default; reserved for future lenient mode)
    #[arg(long)]
    pub strict: bool,

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

pub fn run(args: &ValidateArgs) -> Result<ExitCode> {
    let _ = args.strict; // reserved
    let stdout = io::stdout();
    let handle = stdout.lock();
    let mut writer = BoundedWriter::new(handle, args.limit);

    let mut any_invalid = false;

    if args.files.is_empty() {
        let mut s = String::new();
        io::stdin()
            .read_to_string(&mut s)
            .context("error reading stdin")?;
        match validate_one("<stdin>", &s) {
            Ok(()) => {
                if !args.quiet {
                    writer.write_line("<stdin>: valid")?;
                }
            }
            Err(msg) => {
                any_invalid = true;
                if !args.quiet {
                    eprintln!("{}", msg);
                }
            }
        }
    } else {
        for path in &args.files {
            let name = path.display().to_string();
            let content = match std::fs::read_to_string(path) {
                Ok(s) => s,
                Err(e) => {
                    any_invalid = true;
                    if !args.quiet {
                        eprintln!("{}: cannot read: {}", name, e);
                    }
                    continue;
                }
            };
            match validate_one(&name, &content) {
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
        Ok(ExitCode::from(1))
    } else {
        Ok(ExitCode::SUCCESS)
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
            limit: None,
        };
        let exit = run(&args).unwrap();
        assert_eq!(exit, ExitCode::SUCCESS);
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
            limit: None,
        };
        let exit = run(&args).unwrap();
        assert_eq!(exit, ExitCode::from(1));
    }

    #[test]
    fn validate_one_reports_position() {
        let err = validate_one("x.json", "{\n  bad").unwrap_err();
        assert!(err.starts_with("x.json:"));
    }
}
