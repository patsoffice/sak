use std::io;
use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::Result;
use clap::Args;

use crate::json::read_json_inputs_maybe_lines;
use crate::output::BoundedWriter;
use crate::value::{collect_keys, resolve_expression};

#[derive(Args)]
#[command(
    about = "List keys in a JSON object",
    long_about = "List the keys of a JSON object at a given path.\n\n\
        With no path, lists the top-level keys. With `--depth N`, recursively \
        lists keys up to N levels deep using dot-path notation. With `--types`, \
        each key is annotated with its value type. Reads from stdin if no files \
        are given.\n\n\
        With `--lines`, the input is parsed as NDJSON (one JSON value per line) \
        and keys are listed for each record in turn.",
    after_help = "\
Examples:
  echo '{\"a\":1,\"b\":2}' | sak json keys
  sak json keys data.json                          Top-level keys
  sak json keys .config data.json                  Keys under .config
  sak json keys --depth 2 data.json                Recurse 2 levels
  sak json keys --types data.json                  Show value types
  sak json keys --lines events.ndjson              Keys for each NDJSON record"
)]
pub struct KeysArgs {
    /// Path within the document (default: root)
    pub path: Option<String>,

    /// Input files (reads stdin if omitted)
    pub files: Vec<PathBuf>,

    /// Recurse N levels deep, showing dot-path prefixes
    #[arg(short, long)]
    pub depth: Option<usize>,

    /// Show value type alongside each key
    #[arg(short, long)]
    pub types: bool,

    /// Parse input as NDJSON (one JSON value per line)
    #[arg(long)]
    pub lines: bool,

    /// Maximum number of output lines
    #[arg(long)]
    pub limit: Option<usize>,
}

pub fn run(args: &KeysArgs) -> Result<ExitCode> {
    let inputs = read_json_inputs_maybe_lines(&args.files, args.lines)?;

    let stdout = io::stdout();
    let handle = stdout.lock();
    let mut writer = BoundedWriter::new(handle, args.limit);

    let max_depth = args.depth.unwrap_or(1);
    let mut found_any_object = false;

    for (_name, value) in &inputs {
        let target = match &args.path {
            Some(p) => match resolve_expression(value, p)? {
                Some(v) => v,
                None => continue,
            },
            None => value,
        };

        if !target.is_object() {
            continue;
        }
        found_any_object = true;

        let mut keys = Vec::new();
        collect_keys(target, "", 0, max_depth, args.types, &mut keys);
        for k in keys {
            if !writer.write_line(&k)? {
                writer.flush()?;
                return Ok(ExitCode::SUCCESS);
            }
        }
    }

    writer.flush()?;
    if found_any_object {
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
    fn keys_lines_iterates_records() {
        let (_d, p) = write_tmp("{\"a\":1}\n{\"b\":2}\n");
        let args = KeysArgs {
            path: None,
            files: vec![p],
            depth: None,
            types: false,
            lines: true,
            limit: None,
        };
        assert_eq!(run(&args).unwrap(), ExitCode::SUCCESS);
    }

    #[test]
    fn keys_lines_non_object_records_return_1() {
        let (_d, p) = write_tmp("[1,2]\n3\n");
        let args = KeysArgs {
            path: None,
            files: vec![p],
            depth: None,
            types: false,
            lines: true,
            limit: None,
        };
        assert_eq!(run(&args).unwrap(), ExitCode::from(1));
    }
}
