use crate::output::Outcome;
use std::io::{self, Write};
use std::path::PathBuf;

use anyhow::Result;
use clap::Args;

use crate::config::{Format, convert::emit, detect_format, read_config_value};
use crate::value::{ArrayMerge, MergeOptions, merge_all};

#[derive(Args)]
#[command(
    about = "Preview a layered merge of configs",
    long_about = "Layer one or more override configs onto a base config and \
        print the result — for answering 'what does this config actually \
        look like in production?' without applying anything. Read-only: no \
        file is modified; redirect stdout to write the result.\n\n\
        Objects merge deeply with later values winning at leaves; arrays \
        replace by default (`--arrays append` concatenates); scalars always \
        take the override, even across scalar types. A structural mismatch \
        (object vs array, container vs scalar) is an error naming the path \
        unless `--lenient` lets the override win. Inputs may mix formats \
        (TOML base with YAML overrides); the output defaults to the base \
        file's format, or set it with `--format`.",
    after_help = "\
Examples:
  sak config merge base.toml prod.yaml            Layer prod over base
  sak config merge base.toml a.yaml b.yaml        Later overrides win
  sak config merge --arrays append b.toml c.toml  Concatenate arrays
  sak config merge --format json b.toml c.toml    Emit JSON instead of TOML"
)]
pub struct MergeArgs {
    /// Base config (its format is also the default output format)
    pub base: PathBuf,

    /// Override configs, applied left to right
    #[arg(required = true)]
    pub overrides: Vec<PathBuf>,

    /// How to combine arrays at the same path
    #[arg(long, value_enum, default_value_t = ArrayMerge::Replace)]
    pub arrays: ArrayMerge,

    /// Let the override win on type mismatches instead of erroring
    #[arg(long)]
    pub lenient: bool,

    /// Output format (default: base file's format)
    #[arg(long, value_enum)]
    pub format: Option<Format>,

    /// Input format for all inputs (required when one is stdin)
    #[arg(long, value_enum)]
    pub from: Option<Format>,
}

pub fn run(args: &MergeArgs) -> Result<Outcome> {
    let (_name, base) = read_config_value(&args.base, args.from)?;
    let mut layers = Vec::with_capacity(args.overrides.len());
    for path in &args.overrides {
        let (_name, value) = read_config_value(path, args.from)?;
        layers.push(value);
    }
    let merged = merge_all(
        base,
        layers,
        &MergeOptions {
            arrays: args.arrays,
            lenient: args.lenient,
        },
    )?;

    // Without --format the merged document comes back in the base file's
    // format; stdin has no extension to sniff, so --format is required then.
    let output = match args.format {
        Some(f) => f,
        None => detect_format(&args.base, None)?,
    };
    let stdout = io::stdout();
    let mut handle = stdout.lock();
    // Pretty JSON by default, matching `convert`.
    emit(&merged, output, true, &mut handle)?;
    handle.flush()?;
    Ok(Outcome::Found)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_tmp(name: &str, content: &str) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join(name);
        std::fs::write(&p, content).unwrap();
        (dir, p)
    }

    fn merge_args(base: PathBuf, over: PathBuf) -> MergeArgs {
        MergeArgs {
            base,
            overrides: vec![over],
            arrays: ArrayMerge::Replace,
            lenient: false,
            format: None,
            from: None,
        }
    }

    #[test]
    fn run_layers_yaml_over_toml() {
        let (_d, b) = write_tmp("base.toml", "[server]\nport = 8080\nhost = \"a\"\n");
        let (_d2, o) = write_tmp("over.yaml", "server:\n  port: 9090\n");
        assert_eq!(run(&merge_args(b, o)).unwrap(), Outcome::Found);
    }

    #[test]
    fn run_structural_mismatch_is_tool_error() {
        let (_d, b) = write_tmp("base.toml", "[server]\nport = 8080\n");
        let (_d2, o) = write_tmp("over.toml", "server = [1, 2]\n");
        assert!(run(&merge_args(b, o)).is_err());
    }

    #[test]
    fn run_lenient_allows_mismatch() {
        let (_d, b) = write_tmp("base.toml", "[server]\nport = 8080\n");
        let (_d2, o) = write_tmp("over.toml", "server = [1, 2]\n");
        let mut args = merge_args(b, o);
        args.lenient = true;
        assert_eq!(run(&args).unwrap(), Outcome::Found);
    }

    #[test]
    fn run_format_override_emits_json() {
        let (_d, b) = write_tmp("base.toml", "a = 1\n");
        let (_d2, o) = write_tmp("over.toml", "b = 2\n");
        let mut args = merge_args(b, o);
        args.format = Some(Format::Json);
        assert_eq!(run(&args).unwrap(), Outcome::Found);
    }
}
