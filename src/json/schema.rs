use crate::output::Outcome;
use std::collections::BTreeSet;
use std::io;
use std::path::PathBuf;

use anyhow::Result;
use clap::Args;

use crate::json::read_json_inputs;
use crate::output::BoundedWriter;
use crate::value::{SchemaMap, WalkOpts, collect_schema, format_schema_types, type_name};

#[derive(Args)]
#[command(
    about = "Infer a structural schema from a JSON document",
    long_about = "Walk a JSON document and emit an inferred schema as flat \
        dot-path lines.\n\n\
        Each line has the form `path: type` (or `path: type1|type2` for unions). \
        Array elements use `[]` in the path; for example, `.users[].name: string`. \
        When an array contains heterogeneous element types, the types are merged \
        into a union and the recursive structure of all element schemas is unioned. \
        Reads from stdin if no files are given, or for a file argument of `-`.",
    after_help = "\
Examples:
  echo '{\"a\":1,\"b\":\"x\"}' | sak json schema
  sak json schema data.json
  sak json schema --depth 2 data.json    Limit recursion depth"
)]
pub struct SchemaArgs {
    /// Input files (reads stdin if omitted or given as "-")
    pub files: Vec<PathBuf>,

    /// Maximum nesting depth (deeper structures collapse to their type only)
    #[arg(short, long)]
    pub depth: Option<usize>,

    /// Maximum number of output lines
    #[arg(long)]
    pub limit: Option<usize>,
}

pub fn run(args: &SchemaArgs) -> Result<Outcome> {
    let inputs = read_json_inputs(&args.files)?;

    let stdout = io::stdout();
    let handle = stdout.lock();
    let mut writer = BoundedWriter::new(handle, args.limit);

    let mut any = false;
    for (_name, value) in &inputs {
        let mut schema = SchemaMap::new();

        // Record the root type as "(root): <type>"
        let mut root_types = BTreeSet::new();
        root_types.insert(type_name(value));
        schema.insert(String::new(), root_types);

        collect_schema(value, &WalkOpts::with_max_depth(args.depth), &mut schema);

        for (path, types) in &schema {
            any = true;
            let label = if path.is_empty() {
                "(root)".to_string()
            } else {
                path.clone()
            };
            let line = format!("{}: {}", label, format_schema_types(types));
            if !writer.write_line(&line)? {
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
    use serde_json::{Value, json};

    fn schema_of(v: &Value) -> Vec<String> {
        let mut s = SchemaMap::new();
        let mut roots = BTreeSet::new();
        roots.insert(type_name(v));
        s.insert(String::new(), roots);
        collect_schema(v, &WalkOpts::with_max_depth(None), &mut s);
        s.into_iter()
            .map(|(p, t)| {
                let label = if p.is_empty() {
                    "(root)".to_string()
                } else {
                    p
                };
                format!("{}: {}", label, format_schema_types(&t))
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
        let mut s = SchemaMap::new();
        let mut roots = BTreeSet::new();
        roots.insert(type_name(&v));
        s.insert(String::new(), roots);
        collect_schema(&v, &WalkOpts::with_max_depth(Some(2)), &mut s);
        // depth 2: see .a (depth 1) and .a.b (depth 2), but not .a.b.c
        assert!(s.contains_key(".a"));
        assert!(s.contains_key(".a.b"));
        assert!(!s.contains_key(".a.b.c"));
    }
}
