use std::collections::BTreeMap;
use std::io;
use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{Result, bail};
use clap::Args;

use crate::config::{Format, read_config_inputs};
use crate::output::BoundedWriter;
use crate::value::{ArrayMode, flatten_value};

#[derive(Args)]
#[command(
    about = "Flatten nested TOML/YAML/plist to path/value pairs",
    long_about = "Flatten a nested config document into one line per leaf value, \
        with each line formatted as `path<TAB>value` (matching `json flatten`).\n\n\
        Output is sorted by path for deterministic results. The path separator \
        defaults to `.` and may be customized with `--separator`. Arrays are \
        traversed by default; use `--arrays skip` to treat arrays as leaves. \
        Format is auto-detected from the file extension or set with `--format`. \
        Reads from stdin if no files are given (requires `--format`).",
    after_help = "\
Examples:
  sak config flatten Cargo.toml
  sak config flatten config.yaml
  sak config flatten Info.plist
  sak config flatten --separator / config.toml     Use slash as separator
  sak config flatten --max-depth 2 config.yaml     Stop recursing at depth 2
  sak config flatten --arrays skip config.toml     Treat arrays as leaves"
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

    /// Force a specific format (required for stdin)
    #[arg(short, long, value_enum)]
    pub format: Option<Format>,

    /// Maximum number of output lines
    #[arg(long)]
    pub limit: Option<usize>,
}

pub fn run(args: &FlattenArgs) -> Result<ExitCode> {
    if args.separator.is_empty() {
        bail!("--separator must not be empty");
    }
    let inputs = read_config_inputs(&args.files, args.format)?;

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
    fn flatten_toml() {
        let (_d, p) = write_tmp("a.toml", "[server]\nport = 8080\nhost = \"localhost\"\n");
        let args = FlattenArgs {
            files: vec![p],
            separator: ".".to_string(),
            max_depth: None,
            arrays: ArrayMode::Index,
            format: None,
            limit: None,
        };
        assert_eq!(run(&args).unwrap(), ExitCode::SUCCESS);
    }

    #[test]
    fn flatten_yaml() {
        let (_d, p) = write_tmp("a.yaml", "a:\n  b: 1\n  c: 2\n");
        let args = FlattenArgs {
            files: vec![p],
            separator: ".".to_string(),
            max_depth: None,
            arrays: ArrayMode::Index,
            format: None,
            limit: None,
        };
        assert_eq!(run(&args).unwrap(), ExitCode::SUCCESS);
    }
}
