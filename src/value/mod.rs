//! Format-agnostic helpers that operate on `serde_json::Value`.
//!
//! These utilities are shared by every domain that loads structured data into
//! `serde_json::Value` (currently `json` and `config`). The module is split by
//! the three independent jobs it does, each with its tests alongside it:
//!
//! - [`path`] — parse/resolve input paths (dot notation + JSON Pointer) and
//!   construct output paths ([`build_path`](path::build_path)).
//! - [`diff`] — structural diff machinery ([`diff`], [`DiffEntry`], ...).
//! - [`walk`] — the recursive walkers ([`collect_keys`], [`flatten_value`],
//!   [`collect_schema`], [`grep`]) and their shared [`WalkOpts`].
//!
//! The handful of small, cross-cutting value helpers ([`type_name`],
//! [`value_length`], [`format_value`]) live here in the parent. Everything is
//! re-exported flat, so consumers keep importing from `crate::value::*`.

mod diff;
mod path;
mod walk;

// Flat re-export of each submodule's public surface, so consumers keep
// importing from `crate::value::*` exactly as they did before the split.
// (Glob form also sidesteps the binary-crate "unused re-export" lint for
// items like `PathSegment`/`DiffEntry` that are part of the API but currently
// only referenced within this module.)
pub use diff::*;
pub use path::*;
pub use walk::*;

use serde_json::Value;

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

/// The "length" of a value for the `length` commands.
///
/// Arrays return their element count, objects their key count, strings their
/// Unicode scalar count (not byte length). Scalar number, boolean, and null
/// values have no meaningful length and return `None` — callers should surface
/// this as an error.
pub fn value_length(value: &Value) -> Option<usize> {
    match value {
        Value::Array(a) => Some(a.len()),
        Value::Object(o) => Some(o.len()),
        Value::String(s) => Some(s.chars().count()),
        Value::Null | Value::Bool(_) | Value::Number(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

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
    fn length_of_containers_and_strings() {
        assert_eq!(value_length(&json!([1, 2, 3])), Some(3));
        assert_eq!(value_length(&json!({"a": 1, "b": 2})), Some(2));
        assert_eq!(value_length(&json!("hello")), Some(5));
        assert_eq!(value_length(&json!("")), Some(0));
        assert_eq!(value_length(&json!([])), Some(0));
        assert_eq!(value_length(&json!({})), Some(0));
    }

    #[test]
    fn length_counts_unicode_scalars_not_bytes() {
        // "é" is 2 UTF-8 bytes but 1 char; "🦀" is 4 bytes but 1 char.
        assert_eq!(value_length(&json!("héllo")), Some(5));
        assert_eq!(value_length(&json!("🦀🦀")), Some(2));
    }

    #[test]
    fn length_of_scalars_is_none() {
        assert_eq!(value_length(&json!(null)), None);
        assert_eq!(value_length(&json!(true)), None);
        assert_eq!(value_length(&json!(42)), None);
        assert_eq!(value_length(&json!(2.5)), None);
    }
}
