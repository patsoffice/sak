use std::collections::BTreeMap;
use std::io;
use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{Result, bail};
use clap::{Args, ValueEnum};
use serde_json::Value;

use crate::json::read_json_inputs;
use crate::output::BoundedWriter;

#[derive(Copy, Clone, Debug, PartialEq, Eq, ValueEnum)]
pub enum ArrayMode {
    /// Recurse into arrays using numeric indices in the path
    Index,
    /// Treat arrays as leaf values (do not recurse)
    Skip,
}

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

fn flatten_value(
    value: &Value,
    prefix: &str,
    separator: &str,
    current_depth: usize,
    max_depth: Option<usize>,
    arrays: ArrayMode,
    out: &mut BTreeMap<String, String>,
) {
    let at_max = matches!(max_depth, Some(d) if current_depth >= d);

    match value {
        Value::Object(map) if !at_max => {
            if map.is_empty() {
                out.insert(prefix.to_string(), "{}".to_string());
                return;
            }
            for (k, v) in map {
                let path = if prefix.is_empty() {
                    k.clone()
                } else {
                    format!("{}{}{}", prefix, separator, k)
                };
                flatten_value(
                    v,
                    &path,
                    separator,
                    current_depth + 1,
                    max_depth,
                    arrays,
                    out,
                );
            }
        }
        Value::Array(arr) if !at_max && arrays == ArrayMode::Index => {
            if arr.is_empty() {
                out.insert(prefix.to_string(), "[]".to_string());
                return;
            }
            for (i, v) in arr.iter().enumerate() {
                let path = if prefix.is_empty() {
                    i.to_string()
                } else {
                    format!("{}{}{}", prefix, separator, i)
                };
                flatten_value(
                    v,
                    &path,
                    separator,
                    current_depth + 1,
                    max_depth,
                    arrays,
                    out,
                );
            }
        }
        _ => {
            out.insert(
                prefix.to_string(),
                serde_json::to_string(value).unwrap_or_default(),
            );
        }
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn flatten_object() {
        let v = json!({"a": 1, "b": {"c": 2}});
        let mut out = BTreeMap::new();
        flatten_value(&v, "", ".", 0, None, ArrayMode::Index, &mut out);
        assert_eq!(out.get("a"), Some(&"1".to_string()));
        assert_eq!(out.get("b.c"), Some(&"2".to_string()));
    }

    #[test]
    fn flatten_array_indices() {
        let v = json!({"users": [{"name": "alice"}, {"name": "bob"}]});
        let mut out = BTreeMap::new();
        flatten_value(&v, "", ".", 0, None, ArrayMode::Index, &mut out);
        assert_eq!(out.get("users.0.name"), Some(&"\"alice\"".to_string()));
        assert_eq!(out.get("users.1.name"), Some(&"\"bob\"".to_string()));
    }

    #[test]
    fn flatten_arrays_skip() {
        let v = json!({"a": [1, 2, 3]});
        let mut out = BTreeMap::new();
        flatten_value(&v, "", ".", 0, None, ArrayMode::Skip, &mut out);
        assert_eq!(out.get("a"), Some(&"[1,2,3]".to_string()));
    }

    #[test]
    fn flatten_max_depth() {
        let v = json!({"a": {"b": {"c": 1}}});
        let mut out = BTreeMap::new();
        flatten_value(&v, "", ".", 0, Some(1), ArrayMode::Index, &mut out);
        assert_eq!(out.get("a"), Some(&r#"{"b":{"c":1}}"#.to_string()));
    }

    #[test]
    fn flatten_custom_separator() {
        let v = json!({"a": {"b": 1}});
        let mut out = BTreeMap::new();
        flatten_value(&v, "", "/", 0, None, ArrayMode::Index, &mut out);
        assert_eq!(out.get("a/b"), Some(&"1".to_string()));
    }
}
