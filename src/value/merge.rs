use anyhow::{Result, bail};
use clap::ValueEnum;
use serde_json::Value;

use crate::value::type_name;

/// How to combine two arrays found at the same path during a merge.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, ValueEnum)]
pub enum ArrayMerge {
    /// The override's array replaces the base array.
    #[default]
    Replace,
    /// The override's elements append to the base array's.
    Append,
}

/// Options for [`merge`].
#[derive(Copy, Clone, Debug)]
pub struct MergeOptions {
    pub arrays: ArrayMerge,
    pub lenient: bool,
}

/// Deep-merge `overlay` onto `base`, returning the combined document.
/// Objects merge recursively with later values winning at leaves; arrays
/// replace or append per `opts.arrays`; scalars always take the overlay
/// (overriding a value is the point, even across scalar types). A structural
/// mismatch — object vs array, or a container vs a scalar — is an error naming
/// the path, unless `opts.lenient` lets the overlay win. Pure so the whole
/// merge matrix is unit-testable without I/O.
pub fn merge(base: Value, overlay: Value, opts: &MergeOptions) -> Result<Value> {
    merge_at(base, overlay, opts, "<root>")
}

fn merge_at(base: Value, overlay: Value, opts: &MergeOptions, path: &str) -> Result<Value> {
    match (base, overlay) {
        (Value::Object(mut b), Value::Object(o)) => {
            for (k, v) in o {
                let child = format!("{path}.{k}");
                let merged = match b.remove(&k) {
                    Some(bv) => merge_at(bv, v, opts, &child)?,
                    None => v,
                };
                b.insert(k, merged);
            }
            Ok(Value::Object(b))
        }
        (Value::Array(mut b), Value::Array(o)) => match opts.arrays {
            ArrayMerge::Replace => Ok(Value::Array(o)),
            ArrayMerge::Append => {
                b.extend(o);
                Ok(Value::Array(b))
            }
        },
        (b, o) if !is_container(&b) && !is_container(&o) => Ok(o),
        (b, o) => {
            if opts.lenient {
                Ok(o)
            } else {
                bail!(
                    "type mismatch at {path}: cannot merge {} with {} \
                     (use --lenient to let the override win)",
                    type_name(&b),
                    type_name(&o)
                )
            }
        }
    }
}

fn is_container(v: &Value) -> bool {
    v.is_object() || v.is_array()
}

/// Fold several overlays onto a base left to right. Convenience for the
/// command layer; the per-pair semantics live in [`merge`].
pub fn merge_all(base: Value, overlays: Vec<Value>, opts: &MergeOptions) -> Result<Value> {
    let mut acc = base;
    for overlay in overlays {
        acc = merge(acc, overlay, opts)?;
    }
    Ok(acc)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn strict() -> MergeOptions {
        MergeOptions {
            arrays: ArrayMerge::Replace,
            lenient: false,
        }
    }

    fn opts(arrays: ArrayMerge, lenient: bool) -> MergeOptions {
        MergeOptions { arrays, lenient }
    }

    #[test]
    fn deep_merge_objects_later_wins_at_leaves() {
        let base = json!({"server": {"host": "a", "port": 1}, "kept": true});
        let over = json!({"server": {"port": 2}, "new": 1});
        assert_eq!(
            merge(base, over, &strict()).unwrap(),
            json!({"server": {"host": "a", "port": 2}, "kept": true, "new": 1})
        );
    }

    #[test]
    fn arrays_replace_by_default() {
        let base = json!({"l": [1, 2]});
        let over = json!({"l": [3]});
        assert_eq!(merge(base, over, &strict()).unwrap(), json!({"l": [3]}));
    }

    #[test]
    fn arrays_append_on_request() {
        let base = json!({"l": [1, 2]});
        let over = json!({"l": [3]});
        assert_eq!(
            merge(base, over, &opts(ArrayMerge::Append, false)).unwrap(),
            json!({"l": [1, 2, 3]})
        );
    }

    #[test]
    fn scalars_override_across_types() {
        // Overriding a value is the use case, not a mismatch: no error even
        // when the scalar type changes.
        assert_eq!(merge(json!(1), json!("x"), &strict()).unwrap(), json!("x"));
        assert_eq!(
            merge(json!({"p": 1}), json!({"p": null}), &strict()).unwrap(),
            json!({"p": null})
        );
    }

    #[test]
    fn structural_mismatch_errors_naming_path() {
        let base = json!({"a": {"b": 1}});
        let over = json!({"a": [1]});
        let err = merge(base, over, &strict()).unwrap_err().to_string();
        assert!(err.contains("<root>.a"), "{err}");
        assert!(err.contains("--lenient"), "{err}");
    }

    #[test]
    fn lenient_lets_overlay_win_on_mismatch() {
        let base = json!({"a": {"b": 1}});
        let over = json!({"a": [1]});
        assert_eq!(
            merge(base, over, &opts(ArrayMerge::Replace, true)).unwrap(),
            json!({"a": [1]})
        );
    }

    #[test]
    fn root_mismatch_errors() {
        let err = merge(json!([1]), json!({"a": 1}), &strict())
            .unwrap_err()
            .to_string();
        assert!(err.contains("<root>"), "{err}");
    }

    #[test]
    fn merge_all_folds_left_to_right() {
        let base = json!({"a": 1, "b": 1});
        let outs = merge_all(
            base,
            vec![json!({"a": 2}), json!({"a": 3, "c": 4})],
            &strict(),
        )
        .unwrap();
        assert_eq!(outs, json!({"a": 3, "b": 1, "c": 4}));
    }
}
