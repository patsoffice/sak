//! Path parsing, resolution, and construction over `serde_json::Value`.
//!
//! Two path *syntaxes* are accepted on input — dot notation (`users[0].name`)
//! and JSON Pointer (`/users/0/name`) — auto-detected by [`resolve_expression`].
//! Path *construction* (the inverse: building the dotted path string the
//! walkers emit) goes through [`build_path`] so every walker agrees on one
//! join idiom.

use anyhow::{Context, Result, bail};
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

/// Append `segment` to `prefix` to build an output path, joining with `sep`.
///
/// This is the single join idiom every walker shares ([`collect_keys`],
/// [`flatten_value`], [`collect_schema`], [`diff`], [`grep`] — formerly ~10
/// hand-copied `format!`/`if prefix.is_empty()` sites). The only thing that
/// varies between callers is how the *root* segment is rendered, captured by
/// `leading_sep`:
///
/// - `leading_sep = false` — the root segment stands alone with no separator
///   (`a`, `a.b`, `0`). Used by `keys`/`flatten`/`paths`, whose path style has
///   no leading dot.
/// - `leading_sep = true` — the separator is kept even at the root
///   (`.a`, `.a.b`, `[0]`). Used by `schema`/`diff`/`grep`, whose dotted path
///   style leads with `.` and renders the root as `(root)` downstream.
///
/// `sep` is `""` for array-index segments that already carry their own
/// brackets (e.g. `"[0]"`), so `build_path(".a", "[0]", "", true)` → `".a[0]"`.
///
/// [`collect_keys`]: crate::value::collect_keys
/// [`flatten_value`]: crate::value::flatten_value
/// [`collect_schema`]: crate::value::collect_schema
/// [`diff`]: crate::value::diff
/// [`grep`]: crate::value::grep
pub(crate) fn build_path(prefix: &str, segment: &str, sep: &str, leading_sep: bool) -> String {
    if prefix.is_empty() && !leading_sep {
        segment.to_string()
    } else {
        format!("{prefix}{sep}{segment}")
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
    fn build_path_no_leading_sep_keeps_root_bare() {
        // keys/flatten style: root segment stands alone, deeper levels join.
        assert_eq!(build_path("", "a", ".", false), "a");
        assert_eq!(build_path("a", "b", ".", false), "a.b");
        assert_eq!(build_path("", "0", "/", false), "0");
        assert_eq!(build_path("users", "0", ".", false), "users.0");
    }

    #[test]
    fn build_path_leading_sep_keeps_dotted_root() {
        // schema/diff/grep style: leading separator survives at the root.
        assert_eq!(build_path("", "a", ".", true), ".a");
        assert_eq!(build_path(".a", "b", ".", true), ".a.b");
        // index segments carry their own brackets, so sep is empty.
        assert_eq!(build_path("", "[0]", "", true), "[0]");
        assert_eq!(build_path(".a", "[0]", "", true), ".a[0]");
    }
}
