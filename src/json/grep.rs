use std::io;
use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{Context, Result};
use clap::Args;
use regex::Regex;

use crate::json::read_json_inputs;
use crate::output::BoundedWriter;
use crate::value::{GrepMode, TypeFilter, format_grep_path, grep};

#[derive(Args)]
#[command(
    about = "Find paths whose key or value matches a regex",
    long_about = "Structural search for paths in a JSON document. Unlike \
        `fs grep`, which is text-oriented and can match across comments or \
        produce false positives between keys and values, `json grep` walks \
        the parsed document and emits dot-paths whose object key (default) \
        or scalar leaf value matches the given regex.\n\n\
        Output is `path<TAB>value` lines sorted by path. The empty path \
        (root scalar matches) is rendered as `(root)`. Reads from stdin if \
        no files are given, or for a file argument of `-`.",
    after_help = "\
Examples:
  sak json grep '^aws_' config.json                Keys starting with aws_
  sak json grep -v '@example\\.com$' users.json    Values ending in @example.com
  sak json grep -i password secrets.json           Case-insensitive key match
  sak json grep '.' --type string data.json        All paths with a string value
  sak json grep port --paths-only ports.json       Just the paths, no values"
)]
pub struct GrepArgs {
    /// Regex pattern to match against keys (default) or values (`--value`)
    pub pattern: String,

    /// Input files (reads stdin if omitted or given as "-")
    pub files: Vec<PathBuf>,

    /// Match against object keys (default)
    #[arg(short, long, conflicts_with = "value")]
    pub key: bool,

    /// Match against scalar leaf values instead of keys
    #[arg(short, long)]
    pub value: bool,

    /// Restrict matches to values of this JSON type
    #[arg(short = 't', long = "type", value_enum)]
    pub type_filter: Option<TypeFilter>,

    /// Suppress values from output, emit only paths
    #[arg(long)]
    pub paths_only: bool,

    /// Case-insensitive matching
    #[arg(short = 'i', long = "ignore-case")]
    pub ignore_case: bool,

    /// Maximum number of output lines
    #[arg(long)]
    pub limit: Option<usize>,
}

pub fn run(args: &GrepArgs) -> Result<ExitCode> {
    let inputs = read_json_inputs(&args.files)?;
    let re = build_regex(&args.pattern, args.ignore_case)?;
    let mode = if args.value {
        GrepMode::Values
    } else {
        GrepMode::Keys
    };

    let stdout = io::stdout();
    let handle = stdout.lock();
    let mut writer = BoundedWriter::new(handle, args.limit);

    let mut any = false;
    for (_name, value) in &inputs {
        for (path, val) in grep(value, &re, mode, args.type_filter) {
            any = true;
            let label = format_grep_path(&path);
            let line = if args.paths_only {
                label.to_string()
            } else {
                let rendered = serde_json::to_string(val).unwrap_or_default();
                format!("{}\t{}", label, rendered)
            };
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

fn build_regex(pattern: &str, ignore_case: bool) -> Result<Regex> {
    let full = if ignore_case {
        format!("(?i){}", pattern)
    } else {
        pattern.to_string()
    };
    Regex::new(&full).with_context(|| format!("invalid regex: {}", pattern))
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

    fn args_with(pattern: &str, p: PathBuf) -> GrepArgs {
        GrepArgs {
            pattern: pattern.to_string(),
            files: vec![p],
            key: false,
            value: false,
            type_filter: None,
            paths_only: false,
            ignore_case: false,
            limit: None,
        }
    }

    #[test]
    fn grep_keys_default() {
        let (_d, p) = write_tmp(r#"{"aws_region":"us-east-1","port":80}"#);
        let args = args_with("^aws_", p);
        assert_eq!(run(&args).unwrap(), ExitCode::SUCCESS);
    }

    #[test]
    fn grep_no_match_exits_1() {
        let (_d, p) = write_tmp(r#"{"a":1}"#);
        let args = args_with("nope", p);
        assert_eq!(run(&args).unwrap(), ExitCode::from(1));
    }

    #[test]
    fn grep_values_mode() {
        let (_d, p) = write_tmp(r#"{"email":"alice@example.com","name":"alice"}"#);
        let mut args = args_with("@example\\.com$", p);
        args.value = true;
        assert_eq!(run(&args).unwrap(), ExitCode::SUCCESS);
    }

    #[test]
    fn grep_type_filter() {
        let (_d, p) = write_tmp(r#"{"items":[1,2],"items_count":2}"#);
        let mut args = args_with("^items", p);
        args.type_filter = Some(TypeFilter::Array);
        assert_eq!(run(&args).unwrap(), ExitCode::SUCCESS);
    }

    #[test]
    fn grep_paths_only() {
        let (_d, p) = write_tmp(r#"{"port":8080}"#);
        let mut args = args_with("port", p);
        args.paths_only = true;
        assert_eq!(run(&args).unwrap(), ExitCode::SUCCESS);
    }

    #[test]
    fn grep_ignore_case() {
        let (_d, p) = write_tmp(r#"{"PASSWORD":"hunter2"}"#);
        let mut args = args_with("password", p);
        args.ignore_case = true;
        assert_eq!(run(&args).unwrap(), ExitCode::SUCCESS);
    }

    #[test]
    fn grep_invalid_regex_errors() {
        let (_d, p) = write_tmp(r#"{"a":1}"#);
        let args = args_with("(unclosed", p);
        assert!(run(&args).is_err());
    }
}
