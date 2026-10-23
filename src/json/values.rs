use crate::output::Outcome;
use std::io;
use std::path::PathBuf;

use anyhow::Result;
use clap::Args;
use serde_json::Value;

use crate::json::{read_json_inputs_maybe_lines, table::render_cell};
use crate::output::BoundedWriter;

#[derive(Args)]
#[command(
    about = "Collect every value under a key, wherever nested",
    long_about = "Walk each input document and emit every value found under \
        the given key, regardless of nesting depth — e.g. every 'name' field \
        anywhere in the tree. Both scalar and container values are emitted; \
        the walk continues inside matched subtrees, so a key nested under an \
        already-matched key is emitted too.\n\n\
        Output is `path<TAB>value` lines sorted by path. Paths use `.` \
        separators with array indices as segments (`issues.0.id`). Values \
        render the same way `table` cells do. Key matching is exact.",
    after_help = "\
Examples:
  sak json values name data.json              Every 'name' anywhere in the tree
  sak json values id --lines events.ndjson    Every 'id' in each record"
)]
pub struct ValuesArgs {
    /// Key to collect (exact match, at any depth)
    pub key: String,

    /// Input files (reads stdin if omitted or given as `-`)
    pub files: Vec<PathBuf>,

    /// Parse input as NDJSON (one JSON value per line)
    #[arg(long)]
    pub lines: bool,

    /// Maximum number of output lines
    #[arg(long)]
    pub limit: Option<usize>,
}

/// Walk `node`, pushing `(path, value)` for every object entry whose key
/// matches, descending into matched subtrees too. Pure and order-stable
/// (serde_json maps iterate in key order); callers sort by path.
fn collect(node: &Value, key: &str, prefix: &str, out: &mut Vec<(String, Value)>) {
    match node {
        Value::Object(map) => {
            for (k, v) in map {
                let path = if prefix.is_empty() {
                    k.clone()
                } else {
                    format!("{prefix}.{k}")
                };
                if k == key {
                    out.push((path.clone(), v.clone()));
                }
                collect(v, key, &path, out);
            }
        }
        Value::Array(arr) => {
            for (i, v) in arr.iter().enumerate() {
                let path = if prefix.is_empty() {
                    i.to_string()
                } else {
                    format!("{prefix}.{i}")
                };
                collect(v, key, &path, out);
            }
        }
        _ => {}
    }
}

pub fn run(args: &ValuesArgs) -> Result<Outcome> {
    if args.key.is_empty() {
        anyhow::bail!("key must not be empty");
    }
    let inputs = read_json_inputs_maybe_lines(&args.files, args.lines)?;

    let mut rows: Vec<(String, Value)> = Vec::new();
    for (_name, value) in &inputs {
        collect(value, &args.key, "", &mut rows);
    }
    if rows.is_empty() {
        return Ok(Outcome::NotFound);
    }
    rows.sort_by(|(pa, _), (pb, _)| pa.cmp(pb));

    let stdout = io::stdout();
    let handle = stdout.lock();
    let mut writer = BoundedWriter::new(handle, args.limit);
    for (path, value) in &rows {
        // Values render like `table` cells, keeping the TSV row intact.
        if !writer.write_line(&format!("{path}\t{}", render_cell(Some(value), "-")))? {
            break;
        }
    }
    writer.flush()?;
    Ok(Outcome::Found)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn collected(doc: &Value, key: &str) -> Vec<(String, Value)> {
        let mut out = Vec::new();
        collect(doc, key, "", &mut out);
        out.sort_by(|(pa, _), (pb, _)| pa.cmp(pb));
        out
    }

    #[test]
    fn finds_key_at_every_depth() {
        let doc = json!({"name": "root", "a": {"name": "nested", "list": [{"name": "deep"}]}});
        assert_eq!(
            collected(&doc, "name"),
            vec![
                ("a.list.0.name".to_string(), json!("deep")),
                ("a.name".to_string(), json!("nested")),
                ("name".to_string(), json!("root")),
            ]
        );
    }

    #[test]
    fn matched_container_emits_and_walk_continues_inside() {
        // The outer `name` holds an object (emitted whole) that itself
        // contains `name` (emitted separately).
        let doc = json!({"name": {"name": 1}});
        assert_eq!(
            collected(&doc, "name"),
            vec![
                ("name".to_string(), json!({"name": 1})),
                ("name.name".to_string(), json!(1)),
            ]
        );
    }

    #[test]
    fn no_match_is_empty() {
        let doc = json!({"a": [1, {"b": 2}]});
        assert!(collected(&doc, "zzz").is_empty());
    }

    #[test]
    fn key_match_is_exact() {
        let doc = json!({"name": 1, "names": 2, "Name": 3});
        assert_eq!(
            collected(&doc, "name"),
            vec![("name".to_string(), json!(1))]
        );
    }

    fn write_tmp(content: &str) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.json");
        std::fs::write(&p, content).unwrap();
        (dir, p)
    }

    fn values_args(p: PathBuf, key: &str, lines: bool) -> ValuesArgs {
        ValuesArgs {
            key: key.to_string(),
            files: vec![p],
            lines,
            limit: None,
        }
    }

    #[test]
    fn run_finds_nested() {
        let (_d, p) = write_tmp(r#"{"name":"a","sub":{"name":"b"}}"#);
        assert_eq!(run(&values_args(p, "name", false)).unwrap(), Outcome::Found);
    }

    #[test]
    fn run_no_match_is_no_results() {
        let (_d, p) = write_tmp(r#"{"a":1}"#);
        assert_eq!(
            run(&values_args(p, "zzz", false)).unwrap(),
            Outcome::NotFound
        );
    }

    #[test]
    fn run_lines_mode() {
        let (_d, p) = write_tmp("{\"name\":\"a\"}\n{\"name\":\"b\"}\n");
        assert_eq!(run(&values_args(p, "name", true)).unwrap(), Outcome::Found);
    }

    #[test]
    fn run_empty_key_is_tool_error() {
        let (_d, p) = write_tmp(r#"{"a":1}"#);
        assert!(run(&values_args(p, "", false)).is_err());
    }
}
