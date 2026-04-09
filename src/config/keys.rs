use std::io;
use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::Result;
use clap::Args;

use crate::config::{Format, read_config_inputs};
use crate::output::BoundedWriter;
use crate::value::{collect_keys, resolve_expression};

#[derive(Args)]
#[command(
    about = "List keys in a TOML, YAML, or plist document",
    long_about = "List the keys of a config object at a given path.\n\n\
        With no path, lists the top-level keys. With `--depth N`, recursively \
        lists keys up to N levels deep using dot-path notation. With `--types`, \
        each key is annotated with its value type. Format is auto-detected from \
        the file extension or set with `--format`. Reads from stdin if no files \
        are given (requires `--format`).",
    after_help = "\
Examples:
  sak config keys Cargo.toml                       Top-level keys
  sak config keys .package Cargo.toml              Keys under .package
  sak config keys --depth 2 config.yaml            Recurse 2 levels
  sak config keys --types Info.plist               Show value types"
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

    /// Force a specific format (required for stdin)
    #[arg(short, long, value_enum)]
    pub format: Option<Format>,

    /// Maximum number of output lines
    #[arg(long)]
    pub limit: Option<usize>,
}

pub fn run(args: &KeysArgs) -> Result<ExitCode> {
    let inputs = read_config_inputs(&args.files, args.format)?;

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

    fn write_tmp(name: &str, content: &str) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join(name);
        let mut f = std::fs::File::create(&p).unwrap();
        f.write_all(content.as_bytes()).unwrap();
        (dir, p)
    }

    #[test]
    fn keys_toml_top_level() {
        let (_d, p) = write_tmp("a.toml", "a = 1\nb = 2\n");
        let args = KeysArgs {
            path: None,
            files: vec![p],
            depth: None,
            types: false,
            format: None,
            limit: None,
        };
        assert_eq!(run(&args).unwrap(), ExitCode::SUCCESS);
    }

    #[test]
    fn keys_yaml_with_types() {
        let (_d, p) = write_tmp("a.yaml", "name: alice\nvalues: [1, 2]\n");
        let args = KeysArgs {
            path: None,
            files: vec![p],
            depth: None,
            types: true,
            format: None,
            limit: None,
        };
        assert_eq!(run(&args).unwrap(), ExitCode::SUCCESS);
    }
}
