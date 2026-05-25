use std::io;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, Result};
use clap::Args;
use serde_json::Value;

use crate::json::read_source;
use crate::output::BoundedWriter;
use crate::value::{diff, format_diff_entry};

#[derive(Args)]
#[command(
    about = "Structurally diff two JSON documents",
    long_about = "Compare two JSON documents and report added, removed, and \
        changed paths.\n\n\
        Objects are compared as unordered key sets, arrays as ordered \
        positional sequences. Type mismatches at the same path are reported \
        as `Changed`.\n\n\
        Output format (one line per difference):\n\
        \n  + <path>\\t<value>            added in <b>\
        \n  - <path>\\t<value>            removed from <a>\
        \n  ~ <path>\\t<old> -> <new>     changed\n\n\
        The empty (root) path is rendered as `(root)`. Values are compact JSON.\n\n\
        Exit codes follow `diff(1)` semantics, not sak's usual results-found \
        convention: 0 = identical, 1 = differences found, 2 = error.",
    after_help = "\
Examples:
  sak json diff a.json b.json
  sak json diff config.dev.json config.prod.json --limit 20
  sak json diff old.json new.json && echo identical"
)]
pub struct DiffArgs {
    /// First (left) JSON file (use "-" to read from stdin)
    pub a: PathBuf,
    /// Second (right) JSON file (use "-" to read from stdin)
    pub b: PathBuf,

    /// Maximum number of output lines
    #[arg(long)]
    pub limit: Option<usize>,
}

fn load(path: &Path) -> Result<Value> {
    let (name, s) = read_source(path)?;
    serde_json::from_str(&s).with_context(|| format!("invalid JSON: {}", name))
}

pub fn run(args: &DiffArgs) -> Result<ExitCode> {
    let a = load(&args.a)?;
    let b = load(&args.b)?;

    let entries = diff(&a, &b);
    if entries.is_empty() {
        return Ok(ExitCode::SUCCESS);
    }

    let stdout = io::stdout();
    let handle = stdout.lock();
    let mut writer = BoundedWriter::new(handle, args.limit);
    for entry in &entries {
        if !writer.write_line(&format_diff_entry(entry))? {
            break;
        }
    }
    writer.flush()?;
    Ok(ExitCode::from(1))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_tmp(name: &str, content: &str) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join(name);
        let mut f = std::fs::File::create(&p).unwrap();
        f.write_all(content.as_bytes()).unwrap();
        (dir, p)
    }

    #[test]
    fn diff_identical_returns_zero() {
        let (_d1, a) = write_tmp("a.json", r#"{"x":1}"#);
        let (_d2, b) = write_tmp("b.json", r#"{"x":1}"#);
        let args = DiffArgs { a, b, limit: None };
        assert_eq!(run(&args).unwrap(), ExitCode::SUCCESS);
    }

    #[test]
    fn diff_different_returns_one() {
        let (_d1, a) = write_tmp("a.json", r#"{"x":1}"#);
        let (_d2, b) = write_tmp("b.json", r#"{"x":2}"#);
        let args = DiffArgs { a, b, limit: None };
        assert_eq!(run(&args).unwrap(), ExitCode::from(1));
    }

    #[test]
    fn diff_missing_file_errors() {
        let (_d2, b) = write_tmp("b.json", r#"{}"#);
        let args = DiffArgs {
            a: PathBuf::from("/no/such/file.json"),
            b,
            limit: None,
        };
        assert!(run(&args).is_err());
    }

    #[test]
    fn diff_invalid_json_errors() {
        let (_d1, a) = write_tmp("a.json", "not json");
        let (_d2, b) = write_tmp("b.json", "{}");
        let args = DiffArgs { a, b, limit: None };
        assert!(run(&args).is_err());
    }
}
