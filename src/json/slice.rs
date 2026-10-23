use crate::output::Outcome;
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
    about = "Emit the first N elements of a JSON array",
    long_about = "Emit the first N elements of the array at <path> (default \
        `.`) as a JSON array — for sampling large arrays without piping the \
        whole thing through `query`. Elements from multiple inputs are \
        concatenated in order before the take.",
    after_help = "\
Examples:
  sak json head 5 .users data.json    First 5 users
  sak json head 3 data.json           First 3 elements of a top-level array"
)]
pub struct HeadArgs {
    /// Number of elements to take
    pub n: usize,

    /// Path to the array (dot notation or JSON Pointer; default `.`)
    pub path: Option<String>,

    /// Input files (reads stdin if omitted or given as "-")
    pub files: Vec<PathBuf>,
}

#[derive(Args)]
#[command(
    about = "Emit the last N elements of a JSON array",
    long_about = "Emit the last N elements of the array at <path> (default \
        `.`) as a JSON array — the mirror of `json head`. Elements from \
        multiple inputs are concatenated in order before the take.",
    after_help = "\
Examples:
  sak json tail 5 .users data.json    Last 5 users
  sak json tail 3 data.json           Last 3 elements of a top-level array"
)]
pub struct TailArgs {
    /// Number of elements to take
    pub n: usize,

    /// Path to the array (dot notation or JSON Pointer; default `.`)
    pub path: Option<String>,

    /// Input files (reads stdin if omitted or given as "-")
    pub files: Vec<PathBuf>,
}

#[derive(Args)]
#[command(
    about = "Emit a slice of a JSON array",
    long_about = "Emit elements `[start, end)` of the array at <path> \
        (default `.`) as a JSON array — 0-based, end-exclusive like Python. \
        An omitted end runs to the end of the array. Out-of-range bounds \
        clamp, so over-long slices just return what exists.",
    after_help = "\
Examples:
  sak json slice 10 --end 20 .users data.json   Elements 10-19 of .users
  sak json slice 5 data.json                    Elements 5 to the end"
)]
pub struct SliceArgs {
    /// Start index (0-based, inclusive)
    pub start: usize,

    /// End index (exclusive; default: end of array)
    #[arg(long)]
    pub end: Option<usize>,

    /// Path to the array (dot notation or JSON Pointer; default `.`)
    pub path: Option<String>,

    /// Input files (reads stdin if omitted or given as "-")
    pub files: Vec<PathBuf>,
}

/// Which elements to keep. Pure so the bound logic is unit-testable.
enum Take {
    First(usize),
    Last(usize),
    Range(usize, Option<usize>),
}

fn apply_take(mut elements: Vec<Value>, take: &Take) -> Vec<Value> {
    match *take {
        Take::First(n) => {
            elements.truncate(n);
            elements
        }
        Take::Last(n) => {
            let skip = elements.len().saturating_sub(n);
            elements.into_iter().skip(skip).collect()
        }
        Take::Range(start, end) => {
            let end = end.unwrap_or(usize::MAX);
            // start >= end is an empty slice, not an error.
            if start >= end {
                return Vec::new();
            }
            elements.into_iter().skip(start).take(end - start).collect()
        }
    }
}

/// Resolve `path` in every input and concatenate the array elements in
/// order. A missing path or a non-array is a tool error (exit 2): like
/// `table`, this command needs an array to operate on.
fn collect_elements(inputs: &[(String, Value)], path: &str) -> Result<Vec<Value>> {
    let mut out = Vec::new();
    for (name, value) in inputs {
        match resolve_expression(value, path)? {
            Some(Value::Array(items)) => out.extend(items.iter().cloned()),
            Some(other) => bail!(
                "expected a JSON array at {:?} in {}, found {}",
                path,
                name,
                type_name(other)
            ),
            None => bail!("path {:?} did not resolve in {}", path, name),
        }
    }
    Ok(out)
}

fn run_take(files: &[PathBuf], path: Option<&str>, take: &Take) -> Result<Outcome> {
    // A lone existing file in the path slot is the input (path `.`) —
    // `sak json head 5 data.json` — mirroring `table` and `keys`.
    let (path, files) = crate::input::path_or_file(path, files);
    let array_path = path.as_deref().unwrap_or(".");
    let inputs = read_json_inputs(&files)?;
    let elements = collect_elements(&inputs, array_path)?;
    let kept = apply_take(elements, take);

    let stdout = io::stdout();
    let handle = stdout.lock();
    let mut writer = BoundedWriter::new(handle, None);
    writer.write_line(&serde_json::to_string(&Value::Array(kept)).unwrap_or_default())?;
    writer.flush()?;
    Ok(Outcome::Found)
}

pub fn head(args: &HeadArgs) -> Result<Outcome> {
    run_take(&args.files, args.path.as_deref(), &Take::First(args.n))
}

pub fn tail(args: &TailArgs) -> Result<Outcome> {
    run_take(&args.files, args.path.as_deref(), &Take::Last(args.n))
}

pub fn slice(args: &SliceArgs) -> Result<Outcome> {
    run_take(
        &args.files,
        args.path.as_deref(),
        &Take::Range(args.start, args.end),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn vals(ns: &[i32]) -> Vec<Value> {
        ns.iter().map(|n| json!(n)).collect()
    }

    #[test]
    fn take_first_clamps() {
        assert_eq!(apply_take(vals(&[1, 2, 3]), &Take::First(2)), vals(&[1, 2]));
        assert_eq!(apply_take(vals(&[1]), &Take::First(5)), vals(&[1]));
        assert!(apply_take(vals(&[1]), &Take::First(0)).is_empty());
    }

    #[test]
    fn take_last_clamps() {
        assert_eq!(apply_take(vals(&[1, 2, 3]), &Take::Last(2)), vals(&[2, 3]));
        assert_eq!(apply_take(vals(&[1]), &Take::Last(5)), vals(&[1]));
        assert!(apply_take(vals(&[1, 2]), &Take::Last(0)).is_empty());
    }

    #[test]
    fn take_range_is_start_inclusive_end_exclusive() {
        let all = vals(&[1, 2, 3, 4]);
        assert_eq!(
            apply_take(all.clone(), &Take::Range(1, Some(3))),
            vals(&[2, 3])
        );
        assert_eq!(
            apply_take(all.clone(), &Take::Range(2, None)),
            vals(&[3, 4])
        );
        // Degenerate and out-of-range bounds yield empty, not an error.
        assert!(apply_take(all.clone(), &Take::Range(3, Some(1))).is_empty());
        assert!(apply_take(all.clone(), &Take::Range(9, None)).is_empty());
        assert_eq!(
            apply_take(all, &Take::Range(0, Some(99))),
            vals(&[1, 2, 3, 4])
        );
    }

    #[test]
    fn collect_concatenates_arrays_in_order() {
        let inputs = vec![
            ("a".to_string(), json!([1, 2])),
            ("b".to_string(), json!([3])),
        ];
        assert_eq!(collect_elements(&inputs, ".").unwrap(), vals(&[1, 2, 3]));
    }

    #[test]
    fn collect_rejects_non_array() {
        let inputs = vec![("a".to_string(), json!({"x": 1}))];
        assert!(collect_elements(&inputs, ".").is_err());
        assert!(collect_elements(&inputs, ".missing").is_err());
    }

    fn write_tmp(name: &str, content: &str) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join(name);
        std::fs::write(&p, content).unwrap();
        (dir, p)
    }

    #[test]
    fn run_head_over_path() {
        let (_d, p) = write_tmp("a.json", r#"{"users":[1,2,3,4]}"#);
        let args = HeadArgs {
            n: 2,
            path: Some(".users".to_string()),
            files: vec![p],
        };
        assert_eq!(head(&args).unwrap(), Outcome::Found);
    }

    #[test]
    fn run_tail_defaults_to_root() {
        let (_d, p) = write_tmp("a.json", "[1,2,3]");
        let args = TailArgs {
            n: 2,
            path: None,
            files: vec![p],
        };
        assert_eq!(tail(&args).unwrap(), Outcome::Found);
    }

    #[test]
    fn run_slice_open_end() {
        let (_d, p) = write_tmp("a.json", "[1,2,3,4]");
        let args = SliceArgs {
            start: 2,
            end: None,
            path: None,
            files: vec![p],
        };
        assert_eq!(slice(&args).unwrap(), Outcome::Found);
    }

    #[test]
    fn run_lone_file_positional_is_input() {
        let (_d, p) = write_tmp("a.json", "[1,2,3]");
        // `sak json slice 1 a.json`: the file lands in the path slot.
        let args = SliceArgs {
            start: 1,
            end: Some(2),
            path: Some(p.display().to_string()),
            files: vec![],
        };
        assert_eq!(slice(&args).unwrap(), Outcome::Found);
    }

    #[test]
    fn run_non_array_is_tool_error() {
        let (_d, p) = write_tmp("a.json", r#"{"x":1}"#);
        let args = HeadArgs {
            n: 2,
            path: None,
            files: vec![p],
        };
        assert!(head(&args).is_err());
    }
}
