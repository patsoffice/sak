use crate::output::Outcome;
use std::cmp::Ordering;
use std::io;
use std::path::PathBuf;

use anyhow::{Result, bail};
use clap::Args;
use serde_json::Value;

use crate::json::read_json_inputs;
use crate::output::BoundedWriter;
use crate::value::{resolve_expression, type_name};

#[derive(Args)]
#[command(
    about = "Sort a JSON array by a field",
    long_about = "Sort the array at <path> (default `.`) by the value at \
        <field> in each element, emitting the sorted array as JSON — for \
        ordering query results. The sort is stable: equal keys keep their \
        input order, with --reverse too. Elements missing the field always \
        sort last. Values compare by JSON type rank (null, boolean, number, \
        string, array, object) then by value; with --numeric, numbers and \
        numeric strings compare as f64 and sort before any other value (a \
        `\"NaN\"` string is not numeric). Elements from multiple inputs are \
        concatenated in order before sorting.",
    after_help = "\
Examples:
  sak json sort .id .issues data.json             Sort issues by id
  sak json sort .id --reverse .issues data.json   Newest id first
  sak json sort .count --numeric rows.json         Numeric sort on string counts"
)]
pub struct SortArgs {
    /// Field to sort by (dot notation or JSON Pointer, resolved per element)
    pub field: String,

    /// Path to the array (a lone existing file is read as input, path `.`)
    pub path: Option<String>,

    /// Input files (reads stdin if omitted or given as `-`)
    pub files: Vec<PathBuf>,

    /// Reverse the sort order
    #[arg(long)]
    pub reverse: bool,

    /// Compare numbers and numeric strings as f64
    #[arg(long)]
    pub numeric: bool,
}

/// Rank for cross-type comparison: null, boolean, number, string, array,
/// object — the same order `jq` sorts values in.
fn type_rank(v: &Value) -> u8 {
    match v {
        Value::Null => 0,
        Value::Bool(_) => 1,
        Value::Number(_) => 2,
        Value::String(_) => 3,
        Value::Array(_) => 4,
        Value::Object(_) => 5,
    }
}

/// Best-effort numeric projection for --numeric: JSON numbers directly,
/// strings via f64 parsing. NaN (`"nan"`, `"NaN"`) is *not* numeric: it is
/// unordered against every number, and a comparator that calls it equal to
/// everything isn't a total order (`sort_by` may then misorder or panic).
/// Infinities are ordered and stay numeric.
fn as_number(v: &Value) -> Option<f64> {
    let n = match v {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.parse().ok(),
        _ => None,
    }?;
    (!n.is_nan()).then_some(n)
}

fn scalar_cmp(a: &Value, b: &Value) -> Ordering {
    match (a, b) {
        (Value::Null, Value::Null) => Ordering::Equal,
        (Value::Bool(x), Value::Bool(y)) => x.cmp(y),
        (Value::Number(x), Value::Number(y)) => x
            .as_f64()
            .partial_cmp(&y.as_f64())
            .unwrap_or(Ordering::Equal),
        (Value::String(x), Value::String(y)) => x.cmp(y),
        // Arrays and objects have no natural element-wise order here;
        // canonical JSON text is deterministic and total.
        _ => serde_json::to_string(a)
            .unwrap_or_default()
            .cmp(&serde_json::to_string(b).unwrap_or_default()),
    }
}

pub(crate) fn value_cmp(a: &Value, b: &Value, numeric: bool) -> Ordering {
    if numeric {
        match (as_number(a), as_number(b)) {
            // NaN is filtered out by `as_number`, so `total_cmp` agrees with
            // numeric order here and is total by construction.
            (Some(x), Some(y)) => return x.total_cmp(&y),
            // Numerics sort before anything that isn't a number.
            (Some(_), None) => return Ordering::Less,
            (None, Some(_)) => return Ordering::Greater,
            (None, None) => {}
        }
    }
    type_rank(a)
        .cmp(&type_rank(b))
        .then_with(|| scalar_cmp(a, b))
}

/// Compare two per-element keys. A missing field sorts after every present
/// value in both directions; `reverse` flips only the comparison of present
/// values. Reversing inside the comparator (not reversing the sorted array)
/// is what keeps the sort stable: equal keys still compare Equal, so they
/// keep their input order.
fn key_cmp(a: Option<&Value>, b: Option<&Value>, numeric: bool, reverse: bool) -> Ordering {
    match (a, b) {
        (None, None) => Ordering::Equal,
        (None, _) => Ordering::Greater,
        (_, None) => Ordering::Less,
        (Some(x), Some(y)) => {
            let ord = value_cmp(x, y, numeric);
            if reverse { ord.reverse() } else { ord }
        }
    }
}

/// Stable sort of `(key, element)` pairs, returning the elements. Pure so
/// the ordering (stability, missing-last, reverse) is testable without I/O.
fn sort_by_key(
    mut elements: Vec<(Option<Value>, Value)>,
    numeric: bool,
    reverse: bool,
) -> Vec<Value> {
    elements.sort_by(|(ka, _), (kb, _)| key_cmp(ka.as_ref(), kb.as_ref(), numeric, reverse));
    elements.into_iter().map(|(_, v)| v).collect()
}

pub fn run(args: &SortArgs) -> Result<Outcome> {
    // A lone existing file in the path slot is the input (path `.`) —
    // `sak json sort .age data.json` — mirroring `slice` and `table`.
    let (path, files) = crate::input::path_or_file(args.path.as_deref(), &args.files);
    let array_path = path.as_deref().unwrap_or(".");
    let inputs = read_json_inputs(&files)?;

    let mut elements: Vec<(Option<Value>, Value)> = Vec::new();
    for (name, value) in &inputs {
        match resolve_expression(value, array_path)? {
            Some(Value::Array(items)) => {
                for item in items {
                    let key = resolve_expression(item, &args.field)?.cloned();
                    elements.push((key, item.clone()));
                }
            }
            Some(other) => bail!(
                "expected a JSON array at {:?} in {}, found {}",
                array_path,
                name,
                type_name(other)
            ),
            None => bail!("path {:?} did not resolve in {}", array_path, name),
        }
    }

    let sorted = sort_by_key(elements, args.numeric, args.reverse);
    let stdout = io::stdout();
    let handle = stdout.lock();
    let mut writer = BoundedWriter::new(handle, None);
    writer.write_line(&serde_json::to_string(&Value::Array(sorted)).unwrap_or_default())?;
    writer.flush()?;
    Ok(Outcome::Found)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn cmp(a: &Value, b: &Value, numeric: bool) -> Ordering {
        value_cmp(a, b, numeric)
    }

    #[test]
    fn numbers_compare_numerically_by_default() {
        assert_eq!(cmp(&json!(9), &json!(10), false), Ordering::Less);
    }

    #[test]
    fn numeric_strings_compare_lexicographically_by_default() {
        assert_eq!(cmp(&json!("10"), &json!("9"), false), Ordering::Less);
    }

    #[test]
    fn numeric_flag_compares_numeric_strings_as_numbers() {
        assert_eq!(cmp(&json!("10"), &json!("9"), true), Ordering::Greater);
        assert_eq!(cmp(&json!(9), &json!("10"), true), Ordering::Less);
        // Non-numeric values sort after every number.
        assert_eq!(cmp(&json!("abc"), &json!(1), true), Ordering::Greater);
        assert_eq!(cmp(&json!(true), &json!("2"), true), Ordering::Greater);
    }

    #[test]
    fn cross_type_order_follows_type_rank() {
        assert_eq!(cmp(&json!(null), &json!(false), false), Ordering::Less);
        assert_eq!(cmp(&json!(1), &json!("a"), false), Ordering::Less);
        assert_eq!(cmp(&json!("z"), &json!([1]), false), Ordering::Less);
    }

    #[test]
    fn missing_field_sorts_last() {
        assert_eq!(
            key_cmp(None, Some(&json!(1)), false, false),
            Ordering::Greater
        );
        assert_eq!(key_cmp(Some(&json!(1)), None, false, false), Ordering::Less);
        assert_eq!(key_cmp(None, None, false, false), Ordering::Equal);
    }

    /// `(key, element)` pairs keyed by `.k`, tagged by `.t` for identity.
    fn keyed(rows: &[(Option<i64>, &str)]) -> Vec<(Option<Value>, Value)> {
        rows.iter()
            .map(|(k, t)| (k.map(|k| json!(k)), json!({"t": t})))
            .collect()
    }

    fn tags(sorted: &[Value]) -> Vec<&str> {
        sorted.iter().map(|v| v["t"].as_str().unwrap()).collect()
    }

    #[test]
    fn reverse_keeps_equal_keys_in_input_order_and_missing_last() {
        // Regression: --reverse reversed the sorted array, so equal keys
        // swapped order and missing-field elements jumped to the front.
        let rows = keyed(&[(Some(1), "a"), (None, "m"), (Some(1), "b"), (Some(2), "c")]);
        assert_eq!(
            tags(&sort_by_key(rows.clone(), false, false)),
            ["a", "b", "c", "m"]
        );
        assert_eq!(tags(&sort_by_key(rows, false, true)), ["c", "a", "b", "m"]);
    }

    #[test]
    fn numeric_nan_is_not_a_number() {
        // Regression: "nan" parsed to NaN and compared Equal to every
        // number — not a total order. It now ranks as a non-numeric string.
        assert_eq!(cmp(&json!("nan"), &json!(1), true), Ordering::Greater);
        assert_eq!(cmp(&json!(1), &json!("NaN"), true), Ordering::Less);
        // Infinities stay numeric and ordered.
        assert_eq!(cmp(&json!("inf"), &json!(1e300), true), Ordering::Greater);
        assert_eq!(cmp(&json!("-inf"), &json!(-1), true), Ordering::Less);
    }

    #[test]
    fn numeric_order_is_total_with_nan_strings_mixed_in() {
        // A total order sorts the same however the input is shuffled.
        let values: Vec<Value> = ["3", "nan", "1", "NaN", "2", "x", "nan", "10"]
            .iter()
            .map(|s| json!(s))
            .collect();
        let sorted = |mut v: Vec<Value>| {
            v.sort_by(|a, b| value_cmp(a, b, true));
            v
        };
        let forward = sorted(values.clone());
        let mut backward = values.clone();
        backward.reverse();
        assert_eq!(forward, sorted(backward));
        assert_eq!(
            forward,
            ["1", "2", "3", "10", "NaN", "nan", "nan", "x"].map(|s| json!(s))
        );
    }

    #[test]
    fn complex_values_compare_as_canonical_json() {
        // No element-wise order: canonical text decides ('a' < 'b', '2' < '[').
        assert_eq!(
            cmp(&json!({"a": 1}), &json!({"b": 1}), false),
            Ordering::Less
        );
        assert_eq!(cmp(&json!([2]), &json!([10]), false), Ordering::Greater);
    }

    fn write_tmp(name: &str, content: &str) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join(name);
        std::fs::write(&p, content).unwrap();
        (dir, p)
    }

    fn sort_args(p: PathBuf, reverse: bool) -> SortArgs {
        SortArgs {
            field: ".id".to_string(),
            path: None,
            files: vec![p],
            reverse,
            numeric: false,
        }
    }

    #[test]
    fn run_sorts_by_field() {
        let (_d, p) = write_tmp("a.json", r#"[{"id":2},{"id":1}]"#);
        assert_eq!(run(&sort_args(p, false)).unwrap(), Outcome::Found);
    }

    #[test]
    fn run_reverse_and_missing() {
        let (_d, p) = write_tmp("a.json", r#"[{"id":1},{},{"id":2}]"#);
        assert_eq!(run(&sort_args(p, true)).unwrap(), Outcome::Found);
    }

    #[test]
    fn run_named_file_needs_no_stdin() {
        // Pass the file explicitly: promoting a lone path-slot file
        // to the input depends on the ambient stdin shape
        // (sak-llm-m187); see the slice.rs test.
        let (_d, p) = write_tmp("a.json", r#"[{"id":1}]"#);
        assert_eq!(run(&sort_args(p, false)).unwrap(), Outcome::Found);
    }

    #[test]
    fn run_non_array_is_tool_error() {
        let (_d, p) = write_tmp("a.json", r#"{"x":1}"#);
        let err = run(&sort_args(p, false)).unwrap_err().to_string();
        assert!(err.contains("expected a JSON array"), "{err}");
    }
}
