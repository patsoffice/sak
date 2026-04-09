use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::Result;
use clap::Args;
use serde_json::Value;

use crate::json::read_json_inputs;
use crate::output::BoundedWriter;
use crate::value::type_name;

#[derive(Args)]
#[command(
    about = "Infer a structural schema from a JSON document",
    long_about = "Walk a JSON document and emit an inferred schema as flat \
        dot-path lines.\n\n\
        Each line has the form `path: type` (or `path: type1|type2` for unions). \
        Array elements use `[]` in the path; for example, `.users[].name: string`. \
        When an array contains heterogeneous element types, the types are merged \
        into a union and the recursive structure of all element schemas is unioned. \
        Reads from stdin if no files are given.",
    after_help = "\
Examples:
  echo '{\"a\":1,\"b\":\"x\"}' | sak json schema
  sak json schema data.json
  sak json schema --depth 2 data.json    Limit recursion depth"
)]
pub struct SchemaArgs {
    /// Input files (reads stdin if omitted)
    pub files: Vec<PathBuf>,

    /// Maximum nesting depth (deeper structures collapse to their type only)
    #[arg(short, long)]
    pub depth: Option<usize>,

    /// Maximum number of output lines
    #[arg(long)]
    pub limit: Option<usize>,
}

type Schema = BTreeMap<String, BTreeSet<&'static str>>;

fn collect(
    value: &Value,
    prefix: &str,
    current_depth: usize,
    max_depth: Option<usize>,
    out: &mut Schema,
) {
    let at_max = matches!(max_depth, Some(d) if current_depth >= d);

    match value {
        Value::Object(map) if !at_max => {
            for (k, v) in map {
                let path = format!("{}.{}", prefix, k);
                out.entry(path.clone()).or_default().insert(type_name(v));
                collect(v, &path, current_depth + 1, max_depth, out);
            }
        }
        Value::Array(arr) if !at_max => {
            let path = format!("{}[]", prefix);
            if arr.is_empty() {
                // No element type known; record nothing extra. The parent
                // entry already states "array".
                return;
            }
            for elem in arr {
                out.entry(path.clone()).or_default().insert(type_name(elem));
                collect(elem, &path, current_depth + 1, max_depth, out);
            }
        }
        _ => {}
    }
}

fn format_types(types: &BTreeSet<&'static str>) -> String {
    let v: Vec<&&str> = types.iter().collect();
    v.iter().map(|s| **s).collect::<Vec<_>>().join("|")
}

pub fn run(args: &SchemaArgs) -> Result<ExitCode> {
    let inputs = read_json_inputs(&args.files)?;

    let stdout = io::stdout();
    let handle = stdout.lock();
    let mut writer = BoundedWriter::new(handle, args.limit);

    let mut any = false;
    for (_name, value) in &inputs {
        let mut schema = Schema::new();

        // Record the root type as "(root): <type>"
        let mut root_types = BTreeSet::new();
        root_types.insert(type_name(value));
        schema.insert(String::new(), root_types);

        collect(value, "", 0, args.depth, &mut schema);

        for (path, types) in &schema {
            any = true;
            let label = if path.is_empty() {
                "(root)".to_string()
            } else {
                path.clone()
            };
            let line = format!("{}: {}", label, format_types(types));
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

    fn schema_of(v: &Value) -> Vec<String> {
        let mut s = Schema::new();
        let mut roots = BTreeSet::new();
        roots.insert(type_name(v));
        s.insert(String::new(), roots);
        collect(v, "", 0, None, &mut s);
        s.into_iter()
            .map(|(p, t)| {
                let label = if p.is_empty() {
                    "(root)".to_string()
                } else {
                    p
                };
                format!("{}: {}", label, format_types(&t))
            })
            .collect()
    }

    #[test]
    fn schema_simple_object() {
        let v = json!({"name": "alice", "age": 30});
        let lines = schema_of(&v);
        assert_eq!(
            lines,
            vec![
                "(root): object".to_string(),
                ".age: number".to_string(),
                ".name: string".to_string(),
            ]
        );
    }

    #[test]
    fn schema_nested_object() {
        let v = json!({"a": {"b": 1}});
        let lines = schema_of(&v);
        assert_eq!(
            lines,
            vec![
                "(root): object".to_string(),
                ".a: object".to_string(),
                ".a.b: number".to_string(),
            ]
        );
    }

    #[test]
    fn schema_array_of_objects_unions_keys() {
        let v = json!([{"name": "alice"}, {"name": "bob", "age": 30}]);
        let lines = schema_of(&v);
        assert_eq!(
            lines,
            vec![
                "(root): array".to_string(),
                "[]: object".to_string(),
                "[].age: number".to_string(),
                "[].name: string".to_string(),
            ]
        );
    }

    #[test]
    fn schema_heterogeneous_array() {
        let v = json!([1, "two", true]);
        let lines = schema_of(&v);
        assert_eq!(
            lines,
            vec![
                "(root): array".to_string(),
                "[]: boolean|number|string".to_string(),
            ]
        );
    }

    #[test]
    fn schema_empty_array() {
        let v = json!({"a": []});
        let lines = schema_of(&v);
        assert_eq!(
            lines,
            vec!["(root): object".to_string(), ".a: array".to_string(),]
        );
    }

    #[test]
    fn schema_root_scalar() {
        let v = json!(42);
        let lines = schema_of(&v);
        assert_eq!(lines, vec!["(root): number".to_string()]);
    }

    #[test]
    fn schema_max_depth() {
        let v = json!({"a": {"b": {"c": 1}}});
        let mut s = Schema::new();
        let mut roots = BTreeSet::new();
        roots.insert(type_name(&v));
        s.insert(String::new(), roots);
        collect(&v, "", 0, Some(2), &mut s);
        // depth 2: see .a (depth 1) and .a.b (depth 2), but not .a.b.c
        assert!(s.contains_key(".a"));
        assert!(s.contains_key(".a.b"));
        assert!(!s.contains_key(".a.b.c"));
    }
}
