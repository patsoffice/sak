pub mod flatten;
pub mod keys;
pub mod query;
pub mod validate;

use std::io::{self, Read};
use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{Context, Result, bail};
use clap::Subcommand;
use serde_json::Value;

#[derive(Subcommand)]
pub enum JsonCommand {
    Query(query::QueryArgs),
    Keys(keys::KeysArgs),
    Flatten(flatten::FlattenArgs),
    Validate(validate::ValidateArgs),
}

pub fn run(cmd: &JsonCommand) -> Result<ExitCode> {
    match cmd {
        JsonCommand::Query(args) => query::run(args),
        JsonCommand::Keys(args) => keys::run(args),
        JsonCommand::Flatten(args) => flatten::run(args),
        JsonCommand::Validate(args) => validate::run(args),
    }
}

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

/// Resolve a parsed dot-notation path against a JSON value.
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

/// Resolve an expression against a JSON value, auto-detecting JSON Pointer
/// (leading `/`) vs dot notation.
pub fn resolve_expression<'a>(value: &'a Value, expr: &str) -> Result<Option<&'a Value>> {
    if expr.starts_with('/') || expr.is_empty() {
        Ok(value.pointer(expr))
    } else {
        let segments = parse_dot_path(expr)?;
        Ok(resolve_path(value, &segments))
    }
}

/// Read JSON inputs from the given files, or from stdin if `files` is empty.
/// Returns a vector of `(source_name, value)` pairs.
pub fn read_json_inputs(files: &[PathBuf]) -> Result<Vec<(String, Value)>> {
    let mut out = Vec::new();
    if files.is_empty() {
        let mut s = String::new();
        io::stdin()
            .read_to_string(&mut s)
            .context("error reading stdin")?;
        let value: Value = serde_json::from_str(&s).context("invalid JSON on stdin")?;
        out.push(("<stdin>".to_string(), value));
    } else {
        for path in files {
            let s = std::fs::read_to_string(path)
                .with_context(|| format!("cannot read: {}", path.display()))?;
            let value: Value = serde_json::from_str(&s)
                .with_context(|| format!("invalid JSON: {}", path.display()))?;
            out.push((path.display().to_string(), value));
        }
    }
    Ok(out)
}

/// Format a JSON value for output.
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
}
