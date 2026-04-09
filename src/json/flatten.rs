use std::collections::BTreeMap;
use std::io;
use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{Result, bail};
use clap::Args;

use crate::json::read_json_inputs;
use crate::output::BoundedWriter;
use crate::value::{ArrayMode, flatten_value};

#[derive(Args)]
#[command(
    about = "Flatten nested JSON to path/value pairs",
    long_about = "Flatten a nested JSON document into one line per leaf value, \
        with each line formatted as `path<TAB>value`.\n\n\
        Output is sorted by path for deterministic results. The path separator \
        defaults to `.` and may be customized with `--separator`. Arrays are \
        traversed by default; use `--arrays skip` to treat arrays as leaves. \
        Reads from stdin if no files are given.",
    after_help = "\
Examples:
  echo '{\"a\":{\"b\":1}}' | sak json flatten
  sak json flatten data.json
  sak json flatten --separator / data.json         Use slash as separator
  sak json flatten --max-depth 2 data.json         Stop recursing at depth 2
  sak json flatten --arrays skip data.json         Treat arrays as leaves"
)]
pub struct FlattenArgs {
    /// Input files (reads stdin if omitted)
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

    /// Maximum number of output lines
    #[arg(long)]
    pub limit: Option<usize>,
}

pub fn run(args: &FlattenArgs) -> Result<ExitCode> {
    if args.separator.is_empty() {
        bail!("--separator must not be empty");
    }
    let inputs = read_json_inputs(&args.files)?;

    let stdout = io::stdout();
    let handle = stdout.lock();
    let mut writer = BoundedWriter::new(handle, args.limit);

    let mut any = false;
    for (_name, value) in &inputs {
        let mut out = BTreeMap::new();
        flatten_value(
            value,
            "",
            &args.separator,
            0,
            args.max_depth,
            args.arrays,
            &mut out,
        );
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
