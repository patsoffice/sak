//! Recursive walkers over `serde_json::Value`: key collection, flattening,
//! schema inference, and structural grep.
//!
//! The first three ([`collect_keys`], [`flatten_value`], [`collect_schema`])
//! share the same shape — descend an object/array tree, emit one entry per
//! node, stop at a depth bound — so they take a shared [`WalkOpts`] for their
//! behavior knobs and seed the recursion's traversal state (prefix + current
//! depth) internally rather than exposing it in their signatures. Every path
//! is built through [`super::path::build_path`] for a single join idiom.

use std::collections::{BTreeMap, BTreeSet};

use clap::ValueEnum;
use regex::Regex;
use serde_json::Value;

use super::path::build_path;
use super::type_name;

/// How [`flatten_value`] should treat arrays.
#[derive(Copy, Clone, Debug, PartialEq, Eq, ValueEnum)]
pub enum ArrayMode {
    /// Recurse into arrays using numeric indices in the path
    Index,
    /// Treat arrays as leaf values (do not recurse)
    Skip,
}

/// Behavior configuration shared by the depth-bounded walkers
/// ([`collect_keys`], [`flatten_value`], [`collect_schema`]).
///
/// Bundling these knobs keeps the walker signatures to `(value, &opts, out)`
/// and lets every walker agree on a single `Option<usize>` depth type
/// (`None` = unbounded) instead of the old mix of bare `usize` and
/// `Option<usize>`. Not every field applies to every walker — build one with
/// [`WalkOpts::default`] and set only what the call needs:
///
/// - `max_depth` — all three walkers.
/// - `show_types` — [`collect_keys`] only (append `: <type>` to each key).
/// - `separator` / `arrays` — [`flatten_value`] only.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WalkOpts {
    /// Maximum traversal depth; `None` means unbounded.
    pub max_depth: Option<usize>,
    /// `collect_keys`: append `: <type>` to each emitted key path.
    pub show_types: bool,
    /// `flatten_value`: separator placed between path levels.
    pub separator: String,
    /// `flatten_value`: recurse into arrays by index, or treat them as leaves.
    pub arrays: ArrayMode,
}

impl Default for WalkOpts {
    fn default() -> Self {
        Self {
            max_depth: None,
            show_types: false,
            separator: ".".to_string(),
            arrays: ArrayMode::Index,
        }
    }
}

impl WalkOpts {
    /// Construct options carrying just a depth bound — the common case for
    /// `collect_schema` (and a convenient base for the others).
    pub fn with_max_depth(max_depth: Option<usize>) -> Self {
        Self {
            max_depth,
            ..Self::default()
        }
    }
}

/// Recursively collect the keys of a value's object into `out`, sorted.
///
/// Honors [`WalkOpts::max_depth`] (counting path components: `Some(1)` = top
/// level only, `None` = unbounded) and [`WalkOpts::show_types`].
pub fn collect_keys(value: &Value, opts: &WalkOpts, out: &mut Vec<String>) {
    collect_keys_inner(value, "", 0, opts, out);
}

fn collect_keys_inner(
    value: &Value,
    prefix: &str,
    current_depth: usize,
    opts: &WalkOpts,
    out: &mut Vec<String>,
) {
    if let Value::Object(map) = value {
        let mut entries: Vec<(&String, &Value)> = map.iter().collect();
        entries.sort_by(|a, b| a.0.cmp(b.0));
        for (k, v) in entries {
            let path = build_path(prefix, k, ".", false);
            if opts.show_types {
                out.push(format!("{}: {}", path, type_name(v)));
            } else {
                out.push(path.clone());
            }
            if opts.max_depth.is_none_or(|d| current_depth + 1 < d) {
                collect_keys_inner(v, &path, current_depth + 1, opts, out);
            }
        }
    }
}

/// Flatten a value into `path -> json-encoded scalar` pairs in `out`.
///
/// Honors [`WalkOpts::separator`], [`WalkOpts::arrays`], and
/// [`WalkOpts::max_depth`] (`None` = unbounded).
pub fn flatten_value(value: &Value, opts: &WalkOpts, out: &mut BTreeMap<String, String>) {
    flatten_inner(value, "", 0, opts, out);
}

fn flatten_inner(
    value: &Value,
    prefix: &str,
    current_depth: usize,
    opts: &WalkOpts,
    out: &mut BTreeMap<String, String>,
) {
    let at_max = matches!(opts.max_depth, Some(d) if current_depth >= d);

    match value {
        Value::Object(map) if !at_max => {
            if map.is_empty() {
                out.insert(prefix.to_string(), "{}".to_string());
                return;
            }
            for (k, v) in map {
                let path = build_path(prefix, k, &opts.separator, false);
                flatten_inner(v, &path, current_depth + 1, opts, out);
            }
        }
        Value::Array(arr) if !at_max && opts.arrays == ArrayMode::Index => {
            if arr.is_empty() {
                out.insert(prefix.to_string(), "[]".to_string());
                return;
            }
            for (i, v) in arr.iter().enumerate() {
                let path = build_path(prefix, &i.to_string(), &opts.separator, false);
                flatten_inner(v, &path, current_depth + 1, opts, out);
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
/// Recursion stops at [`WalkOpts::max_depth`] (`None` = unbounded). The caller
/// is responsible for seeding the root entry (e.g., `"" -> type_name(root)`).
pub fn collect_schema(value: &Value, opts: &WalkOpts, out: &mut SchemaMap) {
    collect_schema_inner(value, "", 0, opts, out);
}

fn collect_schema_inner(
    value: &Value,
    prefix: &str,
    current_depth: usize,
    opts: &WalkOpts,
    out: &mut SchemaMap,
) {
    let at_max = matches!(opts.max_depth, Some(d) if current_depth >= d);

    match value {
        Value::Object(map) if !at_max => {
            for (k, v) in map {
                let path = build_path(prefix, k, ".", true);
                out.entry(path.clone()).or_default().insert(type_name(v));
                collect_schema_inner(v, &path, current_depth + 1, opts, out);
            }
        }
        Value::Array(arr) if !at_max => {
            let path = build_path(prefix, "[]", "", true);
            if arr.is_empty() {
                // No element type known; the parent entry already records "array".
                return;
            }
            for elem in arr {
                out.entry(path.clone()).or_default().insert(type_name(elem));
                collect_schema_inner(elem, &path, current_depth + 1, opts, out);
            }
        }
        _ => {}
    }
}

/// Format a set of type names as a pipe-separated union (e.g. `"number|string"`).
pub fn format_schema_types(types: &BTreeSet<&'static str>) -> String {
    types.iter().copied().collect::<Vec<_>>().join("|")
}

/// JSON-type filter for structural grep.
///
/// Variant names map directly to [`type_name`] output (lowercased by clap on the
/// CLI: `--type string`, `--type number`, ...).
#[derive(Copy, Clone, Debug, PartialEq, Eq, ValueEnum)]
pub enum TypeFilter {
    String,
    Number,
    Boolean,
    Null,
    Array,
    Object,
}

impl TypeFilter {
    /// Returns true if `value` is of this type.
    pub fn matches(self, value: &Value) -> bool {
        match self {
            TypeFilter::String => value.is_string(),
            TypeFilter::Number => value.is_number(),
            TypeFilter::Boolean => value.is_boolean(),
            TypeFilter::Null => value.is_null(),
            TypeFilter::Array => value.is_array(),
            TypeFilter::Object => value.is_object(),
        }
    }
}

/// Which axis to match against in [`grep`].
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum GrepMode {
    /// Test the pattern against object keys.
    Keys,
    /// Test the pattern against scalar leaf values.
    Values,
}

/// Walk `value` and return paths whose key or scalar value matches `pattern`.
///
/// In [`GrepMode::Keys`] the pattern is tested against each object key; matching
/// entries emit `(path-to-value, value)`. Array indices are never tested but
/// arrays are still descended into. In [`GrepMode::Values`] the pattern is
/// tested against the string form of each scalar leaf — unquoted for
/// `Value::String`, the JSON text for numbers (`"30"`), booleans (`"true"`),
/// and null (`"null"`). Matching leaves emit `(path-to-leaf, value)`. When
/// `type_filter` is supplied, only matches whose value is of that JSON type
/// are emitted.
///
/// Object keys are visited in sorted order so the returned vector is
/// deterministic and already sorted by path. Paths use the leading-dot dot
/// notation shared with [`format_diff_entry`] and `json schema`
/// (e.g. `.users[0].name`); the empty root path is represented by the empty
/// string and rendered as `(root)` by callers via [`format_grep_path`].
///
/// [`format_diff_entry`]: crate::value::format_diff_entry
pub fn grep<'a>(
    value: &'a Value,
    pattern: &Regex,
    mode: GrepMode,
    type_filter: Option<TypeFilter>,
) -> Vec<(String, &'a Value)> {
    let mut out = Vec::new();
    grep_walk(value, "", pattern, mode, type_filter, &mut out);
    out
}

fn grep_walk<'a>(
    value: &'a Value,
    prefix: &str,
    pattern: &Regex,
    mode: GrepMode,
    type_filter: Option<TypeFilter>,
    out: &mut Vec<(String, &'a Value)>,
) {
    match value {
        Value::Object(map) => {
            let mut entries: Vec<(&String, &Value)> = map.iter().collect();
            entries.sort_by(|a, b| a.0.cmp(b.0));
            for (k, v) in entries {
                let path = build_path(prefix, k, ".", true);
                if mode == GrepMode::Keys
                    && pattern.is_match(k)
                    && type_filter.is_none_or(|t| t.matches(v))
                {
                    out.push((path.clone(), v));
                }
                grep_walk(v, &path, pattern, mode, type_filter, out);
            }
        }
        Value::Array(arr) => {
            for (i, v) in arr.iter().enumerate() {
                let path = build_path(prefix, &format!("[{i}]"), "", true);
                grep_walk(v, &path, pattern, mode, type_filter, out);
            }
        }
        _ => {
            if mode == GrepMode::Values {
                let s = match value {
                    Value::String(s) => s.clone(),
                    Value::Number(n) => n.to_string(),
                    Value::Bool(b) => b.to_string(),
                    Value::Null => "null".to_string(),
                    _ => unreachable!("scalar branch"),
                };
                if pattern.is_match(&s) && type_filter.is_none_or(|t| t.matches(value)) {
                    out.push((prefix.to_string(), value));
                }
            }
        }
    }
}

/// Render a grep result path for output. The empty path is shown as `(root)`
/// (matches [`format_diff_entry`] and `json schema`).
///
/// [`format_diff_entry`]: crate::value::format_diff_entry
pub fn format_grep_path(path: &str) -> &str {
    if path.is_empty() { "(root)" } else { path }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn collect_top_level() {
        let v = json!({"b": 1, "a": 2});
        let mut out = Vec::new();
        collect_keys(&v, &WalkOpts::with_max_depth(Some(1)), &mut out);
        assert_eq!(out, vec!["a".to_string(), "b".to_string()]);
    }

    #[test]
    fn collect_with_types() {
        let v = json!({"a": "x", "b": [1]});
        let mut out = Vec::new();
        let opts = WalkOpts {
            max_depth: Some(1),
            show_types: true,
            ..WalkOpts::default()
        };
        collect_keys(&v, &opts, &mut out);
        assert_eq!(out, vec!["a: string".to_string(), "b: array".to_string()]);
    }

    #[test]
    fn collect_depth_2() {
        let v = json!({"a": {"b": 1}});
        let mut out = Vec::new();
        collect_keys(&v, &WalkOpts::with_max_depth(Some(2)), &mut out);
        assert_eq!(out, vec!["a".to_string(), "a.b".to_string()]);
    }

    #[test]
    fn collect_unbounded_depth() {
        let v = json!({"a": {"b": {"c": 1}}});
        let mut out = Vec::new();
        collect_keys(&v, &WalkOpts::with_max_depth(None), &mut out);
        assert_eq!(
            out,
            vec!["a".to_string(), "a.b".to_string(), "a.b.c".to_string()]
        );
    }

    fn flatten_with(
        v: &Value,
        separator: &str,
        max_depth: Option<usize>,
        arrays: ArrayMode,
    ) -> BTreeMap<String, String> {
        let mut out = BTreeMap::new();
        let opts = WalkOpts {
            max_depth,
            separator: separator.to_string(),
            arrays,
            ..WalkOpts::default()
        };
        flatten_value(v, &opts, &mut out);
        out
    }

    #[test]
    fn flatten_object() {
        let v = json!({"a": 1, "b": {"c": 2}});
        let out = flatten_with(&v, ".", None, ArrayMode::Index);
        assert_eq!(out.get("a"), Some(&"1".to_string()));
        assert_eq!(out.get("b.c"), Some(&"2".to_string()));
    }

    #[test]
    fn flatten_array_indices() {
        let v = json!({"users": [{"name": "alice"}, {"name": "bob"}]});
        let out = flatten_with(&v, ".", None, ArrayMode::Index);
        assert_eq!(out.get("users.0.name"), Some(&"\"alice\"".to_string()));
        assert_eq!(out.get("users.1.name"), Some(&"\"bob\"".to_string()));
    }

    #[test]
    fn flatten_arrays_skip() {
        let v = json!({"a": [1, 2, 3]});
        let out = flatten_with(&v, ".", None, ArrayMode::Skip);
        assert_eq!(out.get("a"), Some(&"[1,2,3]".to_string()));
    }

    #[test]
    fn flatten_max_depth() {
        let v = json!({"a": {"b": {"c": 1}}});
        let out = flatten_with(&v, ".", Some(1), ArrayMode::Index);
        assert_eq!(out.get("a"), Some(&r#"{"b":{"c":1}}"#.to_string()));
    }

    #[test]
    fn flatten_custom_separator() {
        let v = json!({"a": {"b": 1}});
        let out = flatten_with(&v, "/", None, ArrayMode::Index);
        assert_eq!(out.get("a/b"), Some(&"1".to_string()));
    }

    fn schema_of(v: &Value, max_depth: Option<usize>) -> SchemaMap {
        let mut out = SchemaMap::new();
        let mut roots = BTreeSet::new();
        roots.insert(type_name(v));
        out.insert(String::new(), roots);
        collect_schema(v, &WalkOpts::with_max_depth(max_depth), &mut out);
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

    fn grep_paths(
        v: &Value,
        pattern: &str,
        mode: GrepMode,
        type_filter: Option<TypeFilter>,
    ) -> Vec<String> {
        let re = Regex::new(pattern).unwrap();
        grep(v, &re, mode, type_filter)
            .into_iter()
            .map(|(p, _)| p)
            .collect()
    }

    #[test]
    fn grep_keys_top_level() {
        let v = json!({"name": "alice", "age": 30});
        assert_eq!(
            grep_paths(&v, "name", GrepMode::Keys, None),
            vec![".name".to_string()]
        );
    }

    #[test]
    fn grep_keys_nested() {
        let v = json!({"server": {"port": 80}, "client": {"port": 81}});
        let mut paths = grep_paths(&v, "port", GrepMode::Keys, None);
        paths.sort();
        assert_eq!(
            paths,
            vec![".client.port".to_string(), ".server.port".to_string()]
        );
    }

    #[test]
    fn grep_keys_prefix_pattern() {
        let v = json!({"aws_region": "us-east-1", "aws_profile": "dev", "port": 80});
        let paths = grep_paths(&v, "^aws_", GrepMode::Keys, None);
        // sorted order: aws_profile before aws_region
        assert_eq!(
            paths,
            vec![".aws_profile".to_string(), ".aws_region".to_string()]
        );
    }

    #[test]
    fn grep_keys_inside_arrays() {
        let v = json!({"users": [{"name": "alice"}, {"name": "bob"}]});
        let paths = grep_paths(&v, "name", GrepMode::Keys, None);
        assert_eq!(
            paths,
            vec![".users[0].name".to_string(), ".users[1].name".to_string(),]
        );
    }

    #[test]
    fn grep_keys_does_not_match_indices() {
        // Array indices aren't keys: pattern `0` should match the key "0_foo"
        // but never an index even when it's "0".
        let v = json!({"items": [1, 2], "0_foo": "x"});
        let paths = grep_paths(&v, "^0", GrepMode::Keys, None);
        assert_eq!(paths, vec![".0_foo".to_string()]);
    }

    #[test]
    fn grep_values_string_match() {
        let v = json!({"a": "alice", "b": "bob", "c": "alfonso"});
        let paths = grep_paths(&v, "^al", GrepMode::Values, None);
        assert_eq!(paths, vec![".a".to_string(), ".c".to_string()]);
    }

    #[test]
    fn grep_values_string_unquoted() {
        // The pattern is tested against the raw string, not the JSON-encoded
        // form — so `"hello"` (5 chars) should match `^hello$`.
        let v = json!({"greeting": "hello"});
        let paths = grep_paths(&v, "^hello$", GrepMode::Values, None);
        assert_eq!(paths, vec![".greeting".to_string()]);
    }

    #[test]
    fn grep_values_match_numbers_and_bools() {
        let v = json!({"port": 8080, "enabled": true, "missing": null});
        let mut paths = grep_paths(&v, "^(8080|true|null)$", GrepMode::Values, None);
        paths.sort();
        assert_eq!(
            paths,
            vec![
                ".enabled".to_string(),
                ".missing".to_string(),
                ".port".to_string(),
            ]
        );
    }

    #[test]
    fn grep_values_skips_containers() {
        // Pattern would textually match `[1,2]` if we encoded the array,
        // but containers are descended into rather than matched as leaves.
        let v = json!({"a": [1, 2]});
        let paths = grep_paths(&v, ",", GrepMode::Values, None);
        assert!(paths.is_empty());
    }

    #[test]
    fn grep_root_scalar() {
        let v = json!("just a string");
        let paths = grep_paths(&v, "string", GrepMode::Values, None);
        assert_eq!(paths, vec!["".to_string()]);
        assert_eq!(format_grep_path(&paths[0]), "(root)");
    }

    #[test]
    fn grep_type_filter_strings_only() {
        let v = json!({"a": "x", "b": 1, "ax": "y", "bx": 2});
        // Match any key starting with "a" or "b", but only emit strings.
        let paths = grep_paths(&v, "^[ab]", GrepMode::Keys, Some(TypeFilter::String));
        assert_eq!(paths, vec![".a".to_string(), ".ax".to_string()]);
    }

    #[test]
    fn grep_type_filter_arrays_only() {
        let v = json!({"items": [1, 2], "items_count": 2});
        let paths = grep_paths(&v, "^items", GrepMode::Keys, Some(TypeFilter::Array));
        assert_eq!(paths, vec![".items".to_string()]);
    }

    #[test]
    fn grep_match_emits_value_reference() {
        let v = json!({"port": 8080});
        let re = Regex::new("port").unwrap();
        let hits = grep(&v, &re, GrepMode::Keys, None);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].0, ".port");
        assert_eq!(hits[0].1, &json!(8080));
    }

    #[test]
    fn grep_output_sorted_by_path() {
        // Object keys are visited in sorted order — output should already be sorted.
        let v = json!({"z": 1, "m": 2, "a": 3});
        let paths = grep_paths(&v, ".", GrepMode::Keys, None);
        assert_eq!(
            paths,
            vec![".a".to_string(), ".m".to_string(), ".z".to_string()]
        );
    }

    #[test]
    fn format_grep_path_root() {
        assert_eq!(format_grep_path(""), "(root)");
        assert_eq!(format_grep_path(".a"), ".a");
    }
}
