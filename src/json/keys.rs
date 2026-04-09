use std::io;
use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::Result;
use clap::Args;
use serde_json::Value;

use crate::json::{read_json_inputs, resolve_expression, type_name};
use crate::output::BoundedWriter;

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

fn collect_keys(
    value: &Value,
    prefix: &str,
    current_depth: usize,
    max_depth: usize,
    show_types: bool,
    out: &mut Vec<String>,
) {
    if let Value::Object(map) = value {
        let mut entries: Vec<(&String, &Value)> = map.iter().collect();
        entries.sort_by(|a, b| a.0.cmp(b.0));
        for (k, v) in entries {
            let path = if prefix.is_empty() {
                k.clone()
            } else {
                format!("{}.{}", prefix, k)
            };
            if show_types {
                out.push(format!("{}: {}", path, type_name(v)));
            } else {
                out.push(path.clone());
            }
            if current_depth + 1 < max_depth {
                collect_keys(v, &path, current_depth + 1, max_depth, show_types, out);
            }
        }
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn collect_top_level() {
        let v = json!({"b": 1, "a": 2});
        let mut out = Vec::new();
        collect_keys(&v, "", 0, 1, false, &mut out);
        assert_eq!(out, vec!["a".to_string(), "b".to_string()]);
    }

    #[test]
    fn collect_with_types() {
        let v = json!({"a": "x", "b": [1]});
        let mut out = Vec::new();
        collect_keys(&v, "", 0, 1, true, &mut out);
        assert_eq!(out, vec!["a: string".to_string(), "b: array".to_string()]);
    }

    #[test]
    fn collect_depth_2() {
        let v = json!({"a": {"b": 1}});
        let mut out = Vec::new();
        collect_keys(&v, "", 0, 2, false, &mut out);
        assert_eq!(out, vec!["a".to_string(), "a.b".to_string()]);
    }
}
