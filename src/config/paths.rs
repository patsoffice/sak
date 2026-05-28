use crate::output::Outcome;
use std::collections::BTreeMap;
use std::io;
use std::path::PathBuf;

use anyhow::{Result, bail};
use clap::Args;

use crate::config::{Format, read_config_inputs};
use crate::output::BoundedWriter;
use crate::value::{ArrayMode, WalkOpts, flatten_value};

#[derive(Args)]
#[command(
    about = "List all leaf paths in a TOML/YAML/plist document",
    long_about = "List every leaf path in a config document, one per line.\n\n\
        Like `flatten` with the value column omitted — useful when an LLM only \
        needs the structure (e.g., to write code that consumes the config) and \
        not the values. Pairs with `query`: discover paths here, then extract \
        their values.\n\n\
        Output is sorted by path for deterministic results. The path separator \
        defaults to `.` and may be customized with `--separator`. Arrays are \
        traversed by default; use `--arrays skip` to treat arrays as leaves. \
        Format is auto-detected from the file extension or set with `--format`. \
        Reads from stdin if no files are given (requires `--format`).",
    after_help = "\
Examples:
  sak config paths Cargo.toml
  sak config paths config.yaml
  sak config paths Info.plist
  sak config paths --separator / config.toml       Use slash as separator
  sak config paths --max-depth 2 config.yaml       Stop recursing at depth 2
  sak config paths --arrays skip config.toml       Treat arrays as leaves"
)]
pub struct PathsArgs {
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

pub fn run(args: &PathsArgs) -> Result<Outcome> {
    if args.separator.is_empty() {
        bail!("--separator must not be empty");
    }
    let inputs = read_config_inputs(&args.files, args.format)?;

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
    fn paths_toml() {
        let (_d, p) = write_tmp("a.toml", "[server]\nport = 8080\nhost = \"localhost\"\n");
        let args = PathsArgs {
            files: vec![p],
            separator: ".".to_string(),
            max_depth: None,
            arrays: ArrayMode::Index,
            format: None,
            limit: None,
        };
        assert_eq!(run(&args).unwrap(), Outcome::Found);
    }

    #[test]
    fn paths_yaml() {
        let (_d, p) = write_tmp("a.yaml", "a:\n  b: 1\n  c: 2\n");
        let args = PathsArgs {
            files: vec![p],
            separator: ".".to_string(),
            max_depth: None,
            arrays: ArrayMode::Index,
            format: None,
            limit: None,
        };
        assert_eq!(run(&args).unwrap(), Outcome::Found);
    }
}
