use crate::output::Outcome;
use std::collections::BTreeMap;
use std::io;
use std::path::PathBuf;

use anyhow::Result;
use clap::Args;
use serde_json::Value;

use crate::json::{read_json_inputs_maybe_lines, table::render_cell};
use crate::output::BoundedWriter;
use crate::value::resolve_expression;

#[derive(Args)]
#[command(
    about = "Count occurrences of values at a path, or group-by counts",
    long_about = "Count occurrences of values at a path — the frequency table \
        of the json domain, useful for log analysis. Each input document \
        contributes its records (array elements for array roots, the document \
        itself otherwise); the path is resolved in each record, array results \
        are flattened, and every value is tallied. With `--by <field>`, array \
        elements (or records) are grouped by that field instead.\n\n\
        Output is `value<TAB>count` lines sorted by count descending (ties \
        break by value ascending for determinism). Values render the same way \
        `table` renders cells. Records where the path (or the `--by` field) \
        is missing are skipped; explicit JSON nulls count under `-`.\n\n\
        Counts accumulate across all inputs. With `--lines`, the input is \
        parsed as NDJSON (one JSON value per line).",
    after_help = "\
Examples:
  sak json count .level events.ndjson --lines   How many log lines per level
  sak json count .tags data.json                Frequency of each tag in an array
  sak json count .users --by role users.json    Group array elements by field
  some-cmd | sak json count .status             Count values piped on stdin"
)]
pub struct CountArgs {
    /// Path to the value(s) to count (dot notation or JSON Pointer; `.` = root)
    pub path: String,

    /// Input files (reads stdin if omitted or given as "-")
    pub files: Vec<PathBuf>,

    /// Group by this field instead of counting the path value itself
    #[arg(long, value_name = "FIELD")]
    pub by: Option<String>,

    /// Parse input as NDJSON (one JSON value per line)
    #[arg(long)]
    pub lines: bool,

    /// Maximum number of output lines
    #[arg(long)]
    pub limit: Option<usize>,
}

/// Tally group keys over the inputs, sorted by count descending and key
/// ascending. Pure over the parsed inputs so it is unit-testable without I/O.
fn tally(inputs: &[(String, Value)], path: &str, by: Option<&str>) -> Result<Vec<(String, usize)>> {
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for (_name, value) in inputs {
        // Array roots contribute their elements as records; anything else
        // contributes the document itself. This makes whole-document arrays
        // and NDJSON record streams behave identically downstream.
        let records: Vec<&Value> = match value {
            Value::Array(elements) => elements.iter().collect(),
            other => vec![other],
        };
        for record in records {
            let base = match resolve_expression(record, path)? {
                Some(v) => v,
                None => continue,
            };
            let items: Vec<&Value> = match base {
                Value::Array(elements) => elements.iter().collect(),
                other => vec![other],
            };
            for item in items {
                let key_value: Option<&Value> = match by {
                    Some(field) => match resolve_expression(item, field)? {
                        Some(v) => Some(v),
                        None => continue,
                    },
                    None => Some(item),
                };
                // Missing paths never reach render_cell (skipped above);
                // explicit nulls render as the `-` token, as in `table`.
                let key = render_cell(key_value, "-");
                *counts.entry(key).or_default() += 1;
            }
        }
    }
    let mut out: Vec<(String, usize)> = counts.into_iter().collect();
    out.sort_by(|(ka, ca), (kb, cb)| cb.cmp(ca).then_with(|| ka.cmp(kb)));
    Ok(out)
}

pub fn run(args: &CountArgs) -> Result<Outcome> {
    let inputs = read_json_inputs_maybe_lines(&args.files, args.lines)?;
    let rows = tally(&inputs, &args.path, args.by.as_deref())?;
    if rows.is_empty() {
        return Ok(Outcome::NotFound);
    }

    let stdout = io::stdout();
    let handle = stdout.lock();
    let mut writer = BoundedWriter::new(handle, args.limit);
    for (key, count) in &rows {
        if !writer.write_line(&format!("{key}\t{count}"))? {
            break;
        }
    }
    writer.flush()?;
    Ok(Outcome::Found)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn named(values: Vec<Value>) -> Vec<(String, Value)> {
        values
            .into_iter()
            .enumerate()
            .map(|(i, v)| (format!("in:{i}"), v))
            .collect()
    }

    #[test]
    fn tally_counts_scalars_per_record() {
        let inputs = named(vec![
            json!({"level": "info"}),
            json!({"level": "error"}),
            json!({"level": "info"}),
        ]);
        assert_eq!(
            tally(&inputs, ".level", None).unwrap(),
            vec![("info".to_string(), 2), ("error".to_string(), 1)]
        );
    }

    #[test]
    fn tally_sorts_by_count_desc_then_key_asc() {
        let inputs = named(vec![
            json!({"l": "b"}),
            json!({"l": "a"}),
            json!({"l": "c"}),
            json!({"l": "b"}),
            json!({"l": "a"}),
        ]);
        // b and a tie at 2; the key breaks the tie ascending.
        assert_eq!(
            tally(&inputs, ".l", None).unwrap(),
            vec![
                ("a".to_string(), 2),
                ("b".to_string(), 2),
                ("c".to_string(), 1),
            ]
        );
    }

    #[test]
    fn tally_flattens_arrays_at_path() {
        let inputs = named(vec![json!({"tags": ["x", "y", "x"]})]);
        assert_eq!(
            tally(&inputs, ".tags", None).unwrap(),
            vec![("x".to_string(), 2), ("y".to_string(), 1)]
        );
    }

    #[test]
    fn tally_treats_array_roots_as_records() {
        // Whole-document arrays behave like NDJSON record streams.
        let inputs = named(vec![json!([{"s": 1}, {"s": 2}, {"s": 1}])]);
        assert_eq!(
            tally(&inputs, ".s", None).unwrap(),
            vec![("1".to_string(), 2), ("2".to_string(), 1)]
        );
    }

    #[test]
    fn tally_skips_missing_paths() {
        let inputs = named(vec![json!({"a": 1}), json!({"b": 2})]);
        assert_eq!(
            tally(&inputs, ".b", None).unwrap(),
            vec![("2".to_string(), 1)]
        );
        assert!(tally(&inputs, ".nope", None).unwrap().is_empty());
    }

    #[test]
    fn tally_counts_null_under_dash() {
        let inputs = named(vec![json!({"a": null}), json!({"a": null})]);
        assert_eq!(
            tally(&inputs, ".a", None).unwrap(),
            vec![("-".to_string(), 2)]
        );
    }

    #[test]
    fn tally_groups_by_field() {
        let inputs = named(vec![json!([
            {"role": "admin", "id": 1},
            {"role": "viewer", "id": 2},
            {"role": "admin", "id": 3},
            {"id": 4},
        ])]);
        // The record without `role` is skipped, not bucketed.
        assert_eq!(
            tally(&inputs, ".", Some(".role")).unwrap(),
            vec![("admin".to_string(), 2), ("viewer".to_string(), 1)]
        );
    }

    #[test]
    fn tally_accumulates_across_inputs() {
        let inputs = named(vec![json!({"l": "a"}), json!({"l": "a"})]);
        assert_eq!(
            tally(&inputs, ".l", None).unwrap(),
            vec![("a".to_string(), 2)]
        );
    }

    #[test]
    fn tally_renders_complex_values_as_json() {
        let inputs = named(vec![
            json!({"v": {"x": 1}}),
            json!({"v": {"x": 1}}),
            json!({"v": {"x": 2}}),
        ]);
        assert_eq!(
            tally(&inputs, ".v", None).unwrap(),
            vec![("{\"x\":1}".to_string(), 2), ("{\"x\":2}".to_string(), 1),]
        );
    }

    #[test]
    fn tally_flattens_nested_arrays_not_just_top_level() {
        // An array *at the path* flattens even when the root is an object.
        let inputs = named(vec![json!({"v": [1, 2]})]);
        assert_eq!(
            tally(&inputs, ".v", None).unwrap(),
            vec![("1".to_string(), 1), ("2".to_string(), 1)]
        );
    }
}
