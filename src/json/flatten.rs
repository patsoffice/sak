use std::collections::BTreeMap;
use std::io;
use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{Result, bail};
use clap::Args;

use crate::json::read_json_inputs_maybe_lines;
use crate::output::BoundedWriter;
use crate::value::{ArrayMode, WalkOpts, flatten_value};

#[derive(Args)]
#[command(
    about = "Flatten nested JSON to path/value pairs",
    long_about = "Flatten a nested JSON document into one line per leaf value, \
        with each line formatted as `path<TAB>value`.\n\n\
        Output is sorted by path for deterministic results. The path separator \
        defaults to `.` and may be customized with `--separator`. Arrays are \
        traversed by default; use `--arrays skip` to treat arrays as leaves. \
        Reads from stdin if no files are given, or for a file argument of `-`.\n\n\
        With `--lines`, the input is parsed as NDJSON (one JSON value per line) \
        and each record is flattened in turn.",
    after_help = "\
Examples:
  echo '{\"a\":{\"b\":1}}' | sak json flatten
  sak json flatten data.json
  sak json flatten --separator / data.json         Use slash as separator
  sak json flatten --max-depth 2 data.json         Stop recursing at depth 2
  sak json flatten --arrays skip data.json         Treat arrays as leaves
  sak json flatten --lines events.ndjson           Flatten each NDJSON record"
)]
pub struct FlattenArgs {
    /// Input files (reads stdin if omitted or given as "-")
    pub files: Vec<PathBuf>,

    /// Path separator
    #[arg(short, long, default_value = ".")]
    pub separator: String,

    /// Maximum nesting depth (leaves at deeper levels are emitted as-is)
    #[arg(short, long)]
    pub max_depth: Option<usize>,

    /// How to handle arrays
    #[arg(long, value_enum, default_value_t = ArrayMode::Index)]
    pub arrays: ArrayMode,

    /// Parse input as NDJSON (one JSON value per line)
    #[arg(long)]
    pub lines: bool,

    /// Maximum number of output lines
    #[arg(long)]
    pub limit: Option<usize>,
}

pub fn run(args: &FlattenArgs) -> Result<ExitCode> {
    if args.separator.is_empty() {
        bail!("--separator must not be empty");
    }
    let inputs = read_json_inputs_maybe_lines(&args.files, args.lines)?;

    let stdout = io::stdout();
    let handle = stdout.lock();
    let mut writer = BoundedWriter::new(handle, args.limit);

    let opts = WalkOpts {
        max_depth: args.max_depth,
        separator: args.separator.clone(),
        arrays: args.arrays,
        ..WalkOpts::default()
    };
    let mut any = false;
    for (_name, value) in &inputs {
        let mut out = BTreeMap::new();
        flatten_value(value, &opts, &mut out);
        for (path, val) in &out {
            any = true;
            let line = format!("{}\t{}", path, val);
            if !writer.write_line(&line)? {
                writer.flush()?;
                return Ok(ExitCode::SUCCESS);
            }
        }
    }

    writer.flush()?;
    if any {
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
    fn flatten_lines_emits_per_record() {
        let (_d, p) = write_tmp("{\"a\":{\"b\":1}}\n{\"a\":{\"b\":2}}\n");
        let args = FlattenArgs {
            files: vec![p],
            separator: ".".to_string(),
            max_depth: None,
            arrays: ArrayMode::Index,
            lines: true,
            limit: None,
        };
        assert_eq!(run(&args).unwrap(), ExitCode::SUCCESS);
    }
}
