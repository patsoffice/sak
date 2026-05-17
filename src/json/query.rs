use std::io;
use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::Result;
use clap::Args;

use crate::json::read_json_inputs_maybe_lines;
use crate::output::BoundedWriter;
use crate::value::{format_value, resolve_expression};

#[derive(Args)]
#[command(
    about = "Extract values from JSON",
    long_about = "Extract values from JSON using a path expression.\n\n\
        The expression may use dot notation (e.g. `.users[0].name`) or \
        JSON Pointer syntax (e.g. `/users/0/name`). JSON Pointer is detected \
        automatically when the expression starts with `/`. \
        Reads from stdin if no files are given.\n\n\
        With `--lines`, the input is parsed as NDJSON (one JSON value per line) \
        and the expression is applied to each record. Blank lines are skipped. \
        Each line's result is emitted on its own output line, in input order.",
    after_help = "\
Examples:
  echo '{\"name\":\"alice\"}' | sak json query .name
  sak json query '.users[0].name' data.json
  sak json query /users/0/name data.json          JSON Pointer
  sak json query .name --raw data.json            Raw string output
  sak json query .config --pretty data.json       Pretty-print
  sak json query --lines .level events.ndjson     NDJSON: one value per line"
)]
pub struct QueryArgs {
    /// Path expression (dot notation or JSON Pointer)
    pub expression: String,

    /// Input files (reads stdin if omitted)
    pub files: Vec<PathBuf>,

    /// Output raw strings without surrounding quotes
    #[arg(short, long)]
    pub raw: bool,

    /// Compact output (default)
    #[arg(long, conflicts_with = "pretty")]
    pub compact: bool,

    /// Pretty-print output
    #[arg(long)]
    pub pretty: bool,

    /// Parse input as NDJSON (one JSON value per line)
    #[arg(long)]
    pub lines: bool,

    /// Maximum number of output lines
    #[arg(long)]
    pub limit: Option<usize>,
}

pub fn run(args: &QueryArgs) -> Result<ExitCode> {
    let inputs = read_json_inputs_maybe_lines(&args.files, args.lines)?;

    let stdout = io::stdout();
    let handle = stdout.lock();
    let mut writer = BoundedWriter::new(handle, args.limit);

    let mut found_any = false;
    for (_name, value) in &inputs {
        if let Some(result) = resolve_expression(value, &args.expression)? {
            found_any = true;
            let formatted = format_value(result, args.raw, args.pretty);
            for line in formatted.split('\n') {
                if !writer.write_line(line)? {
                    writer.flush()?;
                    return Ok(ExitCode::SUCCESS);
                }
            }
        }
    }

    writer.flush()?;
    if found_any {
        Ok(ExitCode::SUCCESS)
    } else {
        Ok(ExitCode::from(1))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_tmp(content: &str) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.json");
        let mut f = std::fs::File::create(&p).unwrap();
        f.write_all(content.as_bytes()).unwrap();
        (dir, p)
    }

    #[test]
    fn query_simple() {
        let (_d, p) = write_tmp(r#"{"name":"alice","age":30}"#);
        let args = QueryArgs {
            expression: ".name".to_string(),
            files: vec![p],
            raw: false,
            compact: false,
            pretty: false,
            lines: false,
            limit: None,
        };
        assert_eq!(run(&args).unwrap(), ExitCode::SUCCESS);
    }

    #[test]
    fn query_pointer() {
        let (_d, p) = write_tmp(r#"{"a":{"b":1}}"#);
        let args = QueryArgs {
            expression: "/a/b".to_string(),
            files: vec![p],
            raw: false,
            compact: false,
            pretty: false,
            lines: false,
            limit: None,
        };
        assert_eq!(run(&args).unwrap(), ExitCode::SUCCESS);
    }

    #[test]
    fn query_missing_returns_1() {
        let (_d, p) = write_tmp(r#"{"a":1}"#);
        let args = QueryArgs {
            expression: ".missing".to_string(),
            files: vec![p],
            raw: false,
            compact: false,
            pretty: false,
            lines: false,
            limit: None,
        };
        assert_eq!(run(&args).unwrap(), ExitCode::from(1));
    }

    #[test]
    fn query_lines_mode() {
        let (_d, p) =
            write_tmp("{\"name\":\"alice\"}\n{\"name\":\"bob\"}\n\n{\"name\":\"carol\"}\n");
        let args = QueryArgs {
            expression: ".name".to_string(),
            files: vec![p],
            raw: true,
            compact: false,
            pretty: false,
            lines: true,
            limit: None,
        };
        assert_eq!(run(&args).unwrap(), ExitCode::SUCCESS);
    }

    #[test]
    fn query_lines_all_missing_returns_1() {
        let (_d, p) = write_tmp("{\"a\":1}\n{\"a\":2}\n");
        let args = QueryArgs {
            expression: ".missing".to_string(),
            files: vec![p],
            raw: false,
            compact: false,
            pretty: false,
            lines: true,
            limit: None,
        };
        assert_eq!(run(&args).unwrap(), ExitCode::from(1));
    }

    #[test]
    fn query_lines_invalid_record_errors() {
        let (_d, p) = write_tmp("{\"a\":1}\nnot json\n");
        let args = QueryArgs {
            expression: ".a".to_string(),
            files: vec![p],
            raw: false,
            compact: false,
            pretty: false,
            lines: true,
            limit: None,
        };
        let err = run(&args).unwrap_err().to_string();
        assert!(err.contains(":2"), "error should reference line 2: {err}");
    }
}
