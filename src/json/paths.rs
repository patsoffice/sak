use crate::output::Outcome;
use std::collections::BTreeMap;
use std::io;
use std::path::PathBuf;

use anyhow::{Result, bail};
use clap::Args;

use crate::json::read_json_inputs;
use crate::output::BoundedWriter;
use crate::value::{ArrayMode, WalkOpts, flatten_value};

#[derive(Args)]
#[command(
    about = "List all leaf paths in a JSON document",
    long_about = "List every leaf path in a JSON document, one per line.\n\n\
        Like `flatten` with the value column omitted — useful when an LLM only \
        needs the structure (e.g., to write code that consumes the document) \
        and not the values. Pairs with `query`: discover paths here, then \
        extract their values.\n\n\
        Output is sorted by path for deterministic results. The path separator \
        defaults to `.` and may be customized with `--separator`. Arrays are \
        traversed by default; use `--arrays skip` to treat arrays as leaves. \
        Reads from stdin if no files are given, or for a file argument of `-`.",
    after_help = "\
Examples:
  echo '{\"a\":{\"b\":1}}' | sak json paths
  sak json paths data.json
  sak json paths --separator / data.json           Use slash as separator
  sak json paths --max-depth 2 data.json           Stop recursing at depth 2
  sak json paths --arrays skip data.json           Treat arrays as leaves"
)]
pub struct PathsArgs {
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

    /// Maximum number of output lines
    #[arg(long)]
    pub limit: Option<usize>,
}

pub fn run(args: &PathsArgs) -> Result<Outcome> {
    if args.separator.is_empty() {
        bail!("--separator must not be empty");
    }
    let inputs = read_json_inputs(&args.files)?;

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
        for path in out.keys() {
            any = true;
            if !writer.write_line(path)? {
                writer.flush()?;
                return Ok(Outcome::Found);
            }
        }
    }

    writer.flush()?;
    if any {
        Ok(Outcome::Found)
    } else {
        Ok(Outcome::NotFound)
    }
}
