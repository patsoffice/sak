//! Format-agnostic helpers that operate on `serde_json::Value`.
//!
//! These utilities are shared by every domain that loads structured data into
//! `serde_json::Value` (currently `json` and `config`). Path parsing, value
//! resolution, key collection, flattening, and type naming all live here so the
//! command implementations can stay format-neutral.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, Result, bail};
use clap::ValueEnum;
use serde_json::Value;

/// A single segment in a parsed dot-notation path.
#[derive(Debug, PartialEq, Eq)]
pub enum PathSegment {
    Key(String),
    Index(usize),
}

/// Parse a dot-notation path like `users[0].name` or `.users[0].name` into segments.
///
/// Grammar: keys are `[A-Za-z_][A-Za-z0-9_]*`. Indices appear in `[N]` brackets and may
/// follow a key or another index. A leading `.` is allowed and ignored. An empty path
/// (or `.` alone) refers to the root.
pub fn parse_dot_path(expr: &str) -> Result<Vec<PathSegment>> {
    let mut segments = Vec::new();
    let bytes = expr.as_bytes();
    let mut i = 0;

    // Allow optional leading dot
    if i < bytes.len() && bytes[i] == b'.' {
        i += 1;
    }

    while i < bytes.len() {
        if bytes[i] == b'[' {
            // Index segment
            i += 1;
            let start = i;
            while i < bytes.len() && bytes[i].is_ascii_digit() {
                i += 1;
            }
            if start == i {
                bail!("invalid path: expected digit after '[' in '{}'", expr);
            }
            let idx: usize = expr[start..i]
                .parse()
                .with_context(|| format!("invalid index in path '{}'", expr))?;
            if i >= bytes.len() || bytes[i] != b']' {
                bail!("invalid path: expected ']' in '{}'", expr);
            }
            i += 1;
            segments.push(PathSegment::Index(idx));
        } else if bytes[i] == b'.' {
            i += 1;
        } else if bytes[i].is_ascii_alphabetic() || bytes[i] == b'_' {
            let start = i;
            while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
                i += 1;
            }
            segments.push(PathSegment::Key(expr[start..i].to_string()));
        } else {
            bail!(
                "invalid character '{}' in path '{}'",
                bytes[i] as char,
                expr
            );
        }
    }

    Ok(segments)
}

/// Resolve a parsed dot-notation path against a value.
pub fn resolve_path<'a>(value: &'a Value, segments: &[PathSegment]) -> Option<&'a Value> {
    let mut current = value;
    for seg in segments {
        current = match seg {
            PathSegment::Key(k) => current.get(k.as_str())?,
            PathSegment::Index(i) => current.get(*i)?,
        };
    }
    Some(current)
}

/// Resolve an expression against a value, auto-detecting JSON Pointer
/// (leading `/`) vs dot notation.
pub fn resolve_expression<'a>(value: &'a Value, expr: &str) -> Result<Option<&'a Value>> {
    if expr.starts_with('/') || expr.is_empty() {
        Ok(value.pointer(expr))
    } else {
        let segments = parse_dot_path(expr)?;
        Ok(resolve_path(value, &segments))
    }
}

/// Format a value for output.
pub fn format_value(value: &Value, raw: bool, pretty: bool) -> String {
    if raw && let Value::String(s) = value {
        return s.clone();
    }
    if pretty {
        serde_json::to_string_pretty(value).unwrap_or_default()
    } else {
        serde_json::to_string(value).unwrap_or_default()
    }
}

/// Return the JSON type name for a value.
pub fn type_name(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

/// Recursively collect the keys of a value's object into `out`, sorted.
pub fn collect_keys(
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

/// How `flatten_value` should treat arrays.
#[derive(Copy, Clone, Debug, PartialEq, Eq, ValueEnum)]
pub enum ArrayMode {
    /// Recurse into arrays using numeric indices in the path
    Index,
    /// Treat arrays as leaf values (do not recurse)
    Skip,
}

/// Flatten a value into `path -> json-encoded scalar` pairs in `out`.
pub fn flatten_value(
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

/// Mapping from dot-path to the set of types observed at that path.
///
/// Used by the schema commands (`json schema`, `config schema`) to accumulate
/// types across array elements and produce union types where shapes differ.
pub type SchemaMap = BTreeMap<String, BTreeSet<&'static str>>;

/// Walk `value` and populate `out` with the inferred structural schema.
///
/// Each object key becomes a `prefix.key` entry; each array becomes a
/// `prefix[]` entry whose value type is the union of all element types.
/// Recursion stops at `max_depth` (None = unbounded). The caller is responsible
/// for seeding the root entry (e.g., `"" -> type_name(root)`).
pub fn collect_schema(
    value: &Value,
    prefix: &str,
    current_depth: usize,
    max_depth: Option<usize>,
    out: &mut SchemaMap,
) {
    let at_max = matches!(max_depth, Some(d) if current_depth >= d);

    match value {
        Value::Object(map) if !at_max => {
            for (k, v) in map {
                let path = format!("{}.{}", prefix, k);
                out.entry(path.clone()).or_default().insert(type_name(v));
                collect_schema(v, &path, current_depth + 1, max_depth, out);
            }
        }
        Value::Array(arr) if !at_max => {
            let path = format!("{}[]", prefix);
            if arr.is_empty() {
                // No element type known; the parent entry already records "array".
                return;
            }
            for elem in arr {
                out.entry(path.clone()).or_default().insert(type_name(elem));
                collect_schema(elem, &path, current_depth + 1, max_depth, out);
            }
        }
        _ => {}
    }
}

/// Format a set of type names as a pipe-separated union (e.g. `"number|string"`).
pub fn format_schema_types(types: &BTreeSet<&'static str>) -> String {
    types.iter().copied().collect::<Vec<_>>().join("|")
}

/// What kind of change a [`DiffEntry`] represents.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum DiffKind {
    /// Path exists in `b` but not `a`.
    Added,
    /// Path exists in `a` but not `b`.
    Removed,
    /// Path exists in both but the values differ.
    Changed,
}

/// A single structural difference between two values.
#[derive(Debug, PartialEq, Eq)]
pub struct DiffEntry {
    /// Dot-notation path to the differing value. Empty string = root.
    pub path: String,
    pub kind: DiffKind,
    /// `Some` for `Removed` and `Changed`, `None` for `Added`.
    pub old: Option<Value>,
    /// `Some` for `Added` and `Changed`, `None` for `Removed`.
    pub new: Option<Value>,
}

/// Compute a structural diff of two values.
///
/// Objects are compared as unordered key sets; arrays as ordered, positional
/// sequences (mismatched lengths produce `Added`/`Removed` entries at the
/// trailing indices). Type mismatches between non-container values are
/// reported as `Changed`. The returned entries are in depth-first order with
/// object keys visited in sorted order, which is deterministic across runs.
pub fn diff(a: &Value, b: &Value) -> Vec<DiffEntry> {
    let mut out = Vec::new();
    diff_walk(a, b, "", &mut out);
    out
}

fn diff_walk(a: &Value, b: &Value, prefix: &str, out: &mut Vec<DiffEntry>) {
    match (a, b) {
        (Value::Object(ma), Value::Object(mb)) => {
            let mut keys: BTreeSet<&str> = BTreeSet::new();
            for k in ma.keys() {
                keys.insert(k.as_str());
            }
            for k in mb.keys() {
                keys.insert(k.as_str());
            }
            for k in keys {
                let path = format!("{}.{}", prefix, k);
                match (ma.get(k), mb.get(k)) {
                    (Some(va), Some(vb)) => diff_walk(va, vb, &path, out),
                    (None, Some(vb)) => out.push(DiffEntry {
                        path,
                        kind: DiffKind::Added,
                        old: None,
                        new: Some(vb.clone()),
                    }),
                    (Some(va), None) => out.push(DiffEntry {
                        path,
                        kind: DiffKind::Removed,
                        old: Some(va.clone()),
                        new: None,
                    }),
                    (None, None) => {}
                }
            }
        }
        (Value::Array(aa), Value::Array(ab)) => {
            let max = aa.len().max(ab.len());
            for i in 0..max {
                let path = format!("{}[{}]", prefix, i);
                match (aa.get(i), ab.get(i)) {
                    (Some(va), Some(vb)) => diff_walk(va, vb, &path, out),
                    (None, Some(vb)) => out.push(DiffEntry {
                        path,
                        kind: DiffKind::Added,
                        old: None,
                        new: Some(vb.clone()),
                    }),
                    (Some(va), None) => out.push(DiffEntry {
                        path,
                        kind: DiffKind::Removed,
                        old: Some(va.clone()),
                        new: None,
                    }),
                    (None, None) => {}
                }
            }
        }
        _ => {
            if a != b {
                out.push(DiffEntry {
                    path: prefix.to_string(),
                    kind: DiffKind::Changed,
                    old: Some(a.clone()),
                    new: Some(b.clone()),
                });
            }
        }
    }
}

/// Render a [`DiffEntry`] as a single output line. The empty path is shown as
/// `(root)`. Values are emitted as compact JSON. Format:
///
/// - `+ <path>\t<value>`         for `Added`
/// - `- <path>\t<value>`         for `Removed`
/// - `~ <path>\t<old> -> <new>`  for `Changed`
pub fn format_diff_entry(entry: &DiffEntry) -> String {
    let path = if entry.path.is_empty() {
        "(root)"
    } else {
        entry.path.as_str()
    };
    let encode = |v: &Value| serde_json::to_string(v).unwrap_or_default();
    match entry.kind {
        DiffKind::Added => format!("+ {}\t{}", path, encode(entry.new.as_ref().unwrap())),
        DiffKind::Removed => format!("- {}\t{}", path, encode(entry.old.as_ref().unwrap())),
        DiffKind::Changed => format!(
            "~ {}\t{} -> {}",
            path,
            encode(entry.old.as_ref().unwrap()),
            encode(entry.new.as_ref().unwrap())
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parse_simple_key() {
        assert_eq!(
            parse_dot_path("name").unwrap(),
            vec![PathSegment::Key("name".to_string())]
        );
    }

    #[test]
    fn parse_leading_dot() {
        assert_eq!(
            parse_dot_path(".name").unwrap(),
            vec![PathSegment::Key("name".to_string())]
        );
    }

    #[test]
    fn parse_nested() {
        assert_eq!(
            parse_dot_path(".a.b.c").unwrap(),
            vec![
                PathSegment::Key("a".to_string()),
                PathSegment::Key("b".to_string()),
                PathSegment::Key("c".to_string()),
            ]
        );
    }

    #[test]
    fn parse_with_index() {
        assert_eq!(
            parse_dot_path(".users[0].name").unwrap(),
            vec![
                PathSegment::Key("users".to_string()),
                PathSegment::Index(0),
                PathSegment::Key("name".to_string()),
            ]
        );
    }

    #[test]
    fn parse_double_index() {
        assert_eq!(
            parse_dot_path(".matrix[1][2]").unwrap(),
            vec![
                PathSegment::Key("matrix".to_string()),
                PathSegment::Index(1),
                PathSegment::Index(2),
            ]
        );
    }

    #[test]
    fn parse_empty_is_root() {
        assert_eq!(parse_dot_path("").unwrap(), Vec::<PathSegment>::new());
        assert_eq!(parse_dot_path(".").unwrap(), Vec::<PathSegment>::new());
    }

    #[test]
    fn parse_invalid_char() {
        assert!(parse_dot_path(".a-b").is_err());
    }

    #[test]
    fn parse_unclosed_bracket() {
        assert!(parse_dot_path(".a[0").is_err());
    }

    #[test]
    fn resolve_path_basic() {
        let v = json!({"users": [{"name": "alice"}, {"name": "bob"}]});
        let segs = parse_dot_path(".users[1].name").unwrap();
        assert_eq!(resolve_path(&v, &segs), Some(&json!("bob")));
    }

    #[test]
    fn resolve_path_missing() {
        let v = json!({"a": 1});
        let segs = parse_dot_path(".b").unwrap();
        assert_eq!(resolve_path(&v, &segs), None);
    }

    #[test]
    fn resolve_expression_pointer() {
        let v = json!({"a": {"b": 42}});
        assert_eq!(resolve_expression(&v, "/a/b").unwrap(), Some(&json!(42)));
    }

    #[test]
    fn resolve_expression_dot() {
        let v = json!({"a": {"b": 42}});
        assert_eq!(resolve_expression(&v, ".a.b").unwrap(), Some(&json!(42)));
    }

    #[test]
    fn format_value_raw_string() {
        assert_eq!(format_value(&json!("hello"), true, false), "hello");
    }

    #[test]
    fn format_value_quoted_string() {
        assert_eq!(format_value(&json!("hello"), false, false), "\"hello\"");
    }

    #[test]
    fn type_names() {
        assert_eq!(type_name(&json!(null)), "null");
        assert_eq!(type_name(&json!(true)), "boolean");
        assert_eq!(type_name(&json!(1)), "number");
        assert_eq!(type_name(&json!("x")), "string");
        assert_eq!(type_name(&json!([])), "array");
        assert_eq!(type_name(&json!({})), "object");
    }

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

    fn schema_of(v: &Value, max_depth: Option<usize>) -> SchemaMap {
        let mut out = SchemaMap::new();
        let mut roots = BTreeSet::new();
        roots.insert(type_name(v));
        out.insert(String::new(), roots);
        collect_schema(v, "", 0, max_depth, &mut out);
        out
    }

    #[test]
    fn schema_object_keys() {
        let s = schema_of(&json!({"a": 1, "b": "x"}), None);
        assert_eq!(s.get("").unwrap().iter().next(), Some(&"object"));
        assert_eq!(s.get(".a").unwrap().iter().next(), Some(&"number"));
        assert_eq!(s.get(".b").unwrap().iter().next(), Some(&"string"));
    }

    #[test]
    fn schema_array_unions_element_types() {
        let s = schema_of(&json!([1, "x", true]), None);
        let elems: Vec<&&str> = s.get("[]").unwrap().iter().collect();
        assert_eq!(elems, vec![&"boolean", &"number", &"string"]);
    }

    #[test]
    fn schema_empty_array_no_element_entry() {
        let s = schema_of(&json!({"a": []}), None);
        assert!(s.contains_key(".a"));
        assert!(!s.contains_key(".a[]"));
    }

    #[test]
    fn schema_respects_max_depth() {
        let s = schema_of(&json!({"a": {"b": {"c": 1}}}), Some(2));
        assert!(s.contains_key(".a"));
        assert!(s.contains_key(".a.b"));
        assert!(!s.contains_key(".a.b.c"));
    }

    #[test]
    fn format_schema_types_joins_with_pipe() {
        let mut s = BTreeSet::new();
        s.insert("number");
        s.insert("string");
        assert_eq!(format_schema_types(&s), "number|string");
    }

    #[test]
    fn diff_identical_is_empty() {
        let a = json!({"name": "alice", "age": 30});
        let b = a.clone();
        assert!(diff(&a, &b).is_empty());
    }

    #[test]
    fn diff_added_key() {
        let a = json!({"name": "alice"});
        let b = json!({"name": "alice", "age": 30});
        let entries = diff(&a, &b);
        assert_eq!(
            entries,
            vec![DiffEntry {
                path: ".age".to_string(),
                kind: DiffKind::Added,
                old: None,
                new: Some(json!(30)),
            }]
        );
    }

    #[test]
    fn diff_removed_key() {
        let a = json!({"name": "alice", "age": 30});
        let b = json!({"name": "alice"});
        let entries = diff(&a, &b);
        assert_eq!(
            entries,
            vec![DiffEntry {
                path: ".age".to_string(),
                kind: DiffKind::Removed,
                old: Some(json!(30)),
                new: None,
            }]
        );
    }

    #[test]
    fn diff_changed_value() {
        let a = json!({"port": 8080});
        let b = json!({"port": 9090});
        let entries = diff(&a, &b);
        assert_eq!(
            entries,
            vec![DiffEntry {
                path: ".port".to_string(),
                kind: DiffKind::Changed,
                old: Some(json!(8080)),
                new: Some(json!(9090)),
            }]
        );
    }

    #[test]
    fn diff_type_change_is_changed() {
        let a = json!({"x": 1});
        let b = json!({"x": "one"});
        let entries = diff(&a, &b);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].kind, DiffKind::Changed);
        assert_eq!(entries[0].path, ".x");
    }

    #[test]
    fn diff_object_vs_scalar_at_path() {
        // .x changes from object to scalar — Changed at .x, not recursing.
        let a = json!({"x": {"y": 1}});
        let b = json!({"x": 5});
        let entries = diff(&a, &b);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].path, ".x");
        assert_eq!(entries[0].kind, DiffKind::Changed);
    }

    #[test]
    fn diff_nested_paths() {
        let a = json!({"server": {"port": 80, "host": "a"}});
        let b = json!({"server": {"port": 80, "host": "b"}});
        let entries = diff(&a, &b);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].path, ".server.host");
    }

    #[test]
    fn diff_array_element_change() {
        let a = json!([1, 2, 3]);
        let b = json!([1, 9, 3]);
        let entries = diff(&a, &b);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].path, "[1]");
        assert_eq!(entries[0].kind, DiffKind::Changed);
    }

    #[test]
    fn diff_array_length_mismatch_added() {
        let a = json!([1, 2]);
        let b = json!([1, 2, 3]);
        let entries = diff(&a, &b);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].path, "[2]");
        assert_eq!(entries[0].kind, DiffKind::Added);
    }

    #[test]
    fn diff_array_length_mismatch_removed() {
        let a = json!([1, 2, 3]);
        let b = json!([1, 2]);
        let entries = diff(&a, &b);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].path, "[2]");
        assert_eq!(entries[0].kind, DiffKind::Removed);
    }

    #[test]
    fn diff_root_scalar_change() {
        let a = json!(1);
        let b = json!(2);
        let entries = diff(&a, &b);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].path, "");
        assert_eq!(entries[0].kind, DiffKind::Changed);
    }

    #[test]
    fn diff_object_keys_walked_in_sorted_order() {
        // Use add/remove to force a flat output and check ordering.
        let a = json!({});
        let b = json!({"b": 1, "a": 2, "c": 3});
        let entries = diff(&a, &b);
        let paths: Vec<&str> = entries.iter().map(|e| e.path.as_str()).collect();
        assert_eq!(paths, vec![".a", ".b", ".c"]);
    }

    #[test]
    fn diff_cross_format_equivalent() {
        // TOML and YAML that describe the same logical data should diff empty
        // once both are parsed into serde_json::Value. This is the cross-format
        // contract that powers `sak config diff`.
        let toml_v: Value = toml::from_str("name = \"alice\"\nage = 30\n").unwrap();
        let yaml_v: Value = serde_yaml::from_str("name: alice\nage: 30\n").unwrap();
        assert!(diff(&toml_v, &yaml_v).is_empty());
    }

    #[test]
    fn format_diff_entry_added() {
        let e = DiffEntry {
            path: ".x".to_string(),
            kind: DiffKind::Added,
            old: None,
            new: Some(json!(true)),
        };
        assert_eq!(format_diff_entry(&e), "+ .x\ttrue");
    }

    #[test]
    fn format_diff_entry_removed() {
        let e = DiffEntry {
            path: ".x".to_string(),
            kind: DiffKind::Removed,
            old: Some(json!("gone")),
            new: None,
        };
        assert_eq!(format_diff_entry(&e), "- .x\t\"gone\"");
    }

    #[test]
    fn format_diff_entry_changed() {
        let e = DiffEntry {
            path: ".port".to_string(),
            kind: DiffKind::Changed,
            old: Some(json!(8080)),
            new: Some(json!(9090)),
        };
        assert_eq!(format_diff_entry(&e), "~ .port\t8080 -> 9090");
    }

    #[test]
    fn format_diff_entry_root() {
        let e = DiffEntry {
            path: String::new(),
            kind: DiffKind::Changed,
            old: Some(json!(1)),
            new: Some(json!(2)),
        };
        assert_eq!(format_diff_entry(&e), "~ (root)\t1 -> 2");
    }
}
