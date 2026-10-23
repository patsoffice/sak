use crate::output::Outcome;
use std::io;
use std::path::PathBuf;

use anyhow::Result;
use clap::Args;
use serde_json::Value;

use crate::json::read_json_inputs;
use crate::output::BoundedWriter;

#[derive(Args)]
#[command(
    about = "Compact JSON to one line",
    long_about = "Re-parse and re-emit each input as compact JSON — for \
        inlining a pretty-printed document. Object keys sort alphabetically \
        (the deterministic-output convention), so this also normalizes key \
        order. One line per input. `sak json query .` does the same; this \
        verb exists so the operation is discoverable.",
    after_help = "\
Examples:
  sak json minify data.json              Compact a pretty-printed file
  some-cmd | sak json minify             Compact piped JSON"
)]
pub struct MinifyArgs {
    /// Input files (reads stdin if omitted or given as `-`)
    pub files: Vec<PathBuf>,

    /// Maximum number of output lines
    #[arg(long)]
    pub limit: Option<usize>,
}

#[derive(Args)]
#[command(
    about = "Pretty-print JSON",
    long_about = "Re-parse and re-emit each input as indented JSON — for \
        reading a compact document. Object keys sort alphabetically (the \
        deterministic-output convention). `sak json query . --pretty` does \
        the same; this verb exists so the operation is discoverable.",
    after_help = "\
Examples:
  sak json pretty data.json              Expand a compact file
  some-cmd | sak json pretty             Expand piped JSON"
)]
pub struct PrettyArgs {
    /// Input files (reads stdin if omitted or given as `-`)
    pub files: Vec<PathBuf>,

    /// Maximum number of output lines
    #[arg(long)]
    pub limit: Option<usize>,
}

/// Render one parsed document. Pure so the compact/pretty split and key
/// normalization are unit-testable without I/O.
fn format_doc(value: &Value, pretty: bool) -> String {
    if pretty {
        serde_json::to_string_pretty(value).unwrap_or_default()
    } else {
        serde_json::to_string(value).unwrap_or_default()
    }
}

fn run_format(files: &[PathBuf], pretty: bool, limit: Option<usize>) -> Result<Outcome> {
    let inputs = read_json_inputs(files)?;

    let stdout = io::stdout();
    let handle = stdout.lock();
    let mut writer = BoundedWriter::new(handle, limit);
    for (_name, value) in &inputs {
        for line in format_doc(value, pretty).lines() {
            if !writer.write_line(line)? {
                writer.flush()?;
                return Ok(Outcome::Found);
            }
        }
    }
    writer.flush()?;
    Ok(Outcome::Found)
}

pub fn minify(args: &MinifyArgs) -> Result<Outcome> {
    run_format(&args.files, false, args.limit)
}

pub fn pretty(args: &PrettyArgs) -> Result<Outcome> {
    run_format(&args.files, true, args.limit)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn minify_compacts_and_sorts_keys() {
        let v = json!({"b": 1, "a": [1, 2]});
        assert_eq!(format_doc(&v, false), r#"{"a":[1,2],"b":1}"#);
    }

    #[test]
    fn pretty_expands_and_sorts_keys() {
        let v = json!({"b": 1, "a": 2});
        assert_eq!(format_doc(&v, true), "{\n  \"a\": 2,\n  \"b\": 1\n}");
    }

    fn write_tmp(content: &str) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.json");
        std::fs::write(&p, content).unwrap();
        (dir, p)
    }

    #[test]
    fn run_minify() {
        let (_d, p) = write_tmp("{\n  \"a\": 1\n}\n");
        let args = MinifyArgs {
            files: vec![p],
            limit: None,
        };
        assert_eq!(minify(&args).unwrap(), Outcome::Found);
    }

    #[test]
    fn run_pretty() {
        let (_d, p) = write_tmp(r#"{"a":1}"#);
        let args = PrettyArgs {
            files: vec![p],
            limit: None,
        };
        assert_eq!(pretty(&args).unwrap(), Outcome::Found);
    }

    #[test]
    fn run_invalid_json_is_tool_error() {
        let (_d, p) = write_tmp(r#"{"a":}"#);
        let args = MinifyArgs {
            files: vec![p],
            limit: None,
        };
        assert!(minify(&args).is_err());
    }
}
