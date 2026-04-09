//! Format-agnostic helpers that operate on `serde_json::Value`.
//!
//! These utilities are shared by every domain that loads structured data into
//! `serde_json::Value` (currently `json` and `config`). Path parsing, value
//! resolution, key collection, flattening, and type naming all live here so the
//! command implementations can stay format-neutral.

use std::collections::BTreeMap;

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
}
