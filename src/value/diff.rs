//! Structural diff of two `serde_json::Value`s.
//!
//! Drives both `sak json diff` and `sak config diff` (cross-format diffs fall
//! out for free because every format normalizes through `serde_json::Value`).
//! Paths use the leading-dot dotted style (`.server.host`, `[2]`) shared with
//! `schema`/`grep`, built via [`super::path::build_path`].

use std::collections::BTreeSet;

use serde_json::Value;

use super::path::build_path;

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
                let path = build_path(prefix, k, ".", true);
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
                let path = build_path(prefix, &format!("[{i}]"), "", true);
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
