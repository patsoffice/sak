use std::io;
use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::Result;
use clap::Args;

use crate::json::read_json_inputs;
use crate::output::BoundedWriter;
use crate::value::{collect_keys, resolve_expression};

#[derive(Args)]
#[command(
    about = "List keys in a JSON object",
    long_about = "List the keys of a JSON object at a given path.\n\n\
        With no path, lists the top-level keys. With `--depth N`, recursively \
        lists keys up to N levels deep using dot-path notation. With `--types`, \
        each key is annotated with its value type. Reads from stdin if no files \
        are given.",
    after_help = "\
Examples:
  echo '{\"a\":1,\"b\":2}' | sak json keys
  sak json keys data.json                          Top-level keys
  sak json keys .config data.json                  Keys under .config
  sak json keys --depth 2 data.json                Recurse 2 levels
  sak json keys --types data.json                  Show value types"
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

    /// Maximum number of output lines
    #[arg(long)]
    pub limit: Option<usize>,
}

pub fn run(args: &KeysArgs) -> Result<ExitCode> {
    let inputs = read_json_inputs(&args.files)?;

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
