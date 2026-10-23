use crate::output::Outcome;
use std::io::{self, BufRead, BufReader, Read};
use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::Args;
use serde_json::Value;

use crate::input::require_piped_stdin;
use crate::json::is_stdin;
use crate::output::BoundedWriter;

#[derive(Args)]
#[command(
    about = "Report size statistics for a JSON document",
    long_about = "Report size statistics for a JSON document — a quick 'is \
        this file too big to inline?' check before reading the whole thing. \
        Emits one TSV row per input with the raw byte size, the max nesting \
        depth (scalars are 0, each array/object level adds 1), the total \
        scalar leaf count, the longest array length (0 when there are no \
        arrays), and the total object key count including nested objects.\n\n\
        With `--lines`, the input is parsed as NDJSON (one JSON value per \
        line) and each record gets its own row.",
    after_help = "\
Examples:
  sak json size data.json                Size stats for one document
  sak json size a.json b.json            One row per file
  sak json size --lines events.ndjson    One row per record"
)]
pub struct SizeArgs {
    /// Input files (reads stdin if omitted or given as "-")
    pub files: Vec<PathBuf>,

    /// Parse input as NDJSON (one JSON value per line)
    #[arg(long)]
    pub lines: bool,

    /// Maximum number of output lines
    #[arg(long)]
    pub limit: Option<usize>,
}

/// Depth, leaf count, longest array, and total key count for one value.
/// Depth counts nesting levels: scalars are 0, `{"a":1}` is 1,
/// `{"a":{"b":[1]}}` is 3. Pure so it is unit-testable without I/O.
fn stats_of(value: &Value) -> (usize, usize, usize, usize) {
    match value {
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => (0, 1, 0, 0),
        Value::Array(elements) => {
            let mut depth = 1;
            let mut leaves = 0;
            let mut max_array = elements.len();
            let mut keys = 0;
            for el in elements {
                let (d, l, m, k) = stats_of(el);
                depth = depth.max(d + 1);
                leaves += l;
                max_array = max_array.max(m);
                keys += k;
            }
            (depth, leaves, max_array, keys)
        }
        Value::Object(map) => {
            let mut depth = 1;
            let mut leaves = 0;
            let mut max_array = 0;
            let mut keys = map.len();
            for v in map.values() {
                let (d, l, m, k) = stats_of(v);
                depth = depth.max(d + 1);
                leaves += l;
                max_array = max_array.max(m);
                keys += k;
            }
            (depth, leaves, max_array, keys)
        }
    }
}

/// Read raw inputs, keeping the original text for the byte-size stat.
/// Mirrors `read_json_inputs` / `read_json_inputs_lines` naming (`<stdin>`
/// and `file:lineno`) but retains each document's bytes.
fn read_raw_inputs(files: &[PathBuf], lines: bool) -> Result<Vec<(String, String)>> {
    if lines {
        let mut out = Vec::new();
        if files.is_empty() {
            require_piped_stdin()?;
            read_raw_lines_into("<stdin>", io::stdin().lock(), &mut out)?;
        } else {
            for path in files {
                if is_stdin(path) {
                    read_raw_lines_into("<stdin>", io::stdin().lock(), &mut out)?;
                } else {
                    let file = std::fs::File::open(path)
                        .with_context(|| format!("cannot read: {}", path.display()))?;
                    read_raw_lines_into(
                        &path.display().to_string(),
                        BufReader::new(file),
                        &mut out,
                    )?;
                }
            }
        }
        return Ok(out);
    }

    if files.is_empty() {
        require_piped_stdin()?;
        let mut s = String::new();
        io::stdin()
            .read_to_string(&mut s)
            .context("error reading stdin")?;
        return Ok(vec![("<stdin>".to_string(), s)]);
    }
    let mut out = Vec::new();
    for path in files {
        if is_stdin(path) {
            let mut s = String::new();
            io::stdin()
                .read_to_string(&mut s)
                .context("error reading stdin")?;
            out.push(("<stdin>".to_string(), s));
        } else {
            let s = std::fs::read_to_string(path)
                .with_context(|| format!("cannot read: {}", path.display()))?;
            out.push((path.display().to_string(), s));
        }
    }
    Ok(out)
}

fn read_raw_lines_into<R: BufRead>(
    name: &str,
    reader: R,
    out: &mut Vec<(String, String)>,
) -> Result<()> {
    for (idx, line) in reader.lines().enumerate() {
        let line = line.with_context(|| format!("error reading {}", name))?;
        if line.trim().is_empty() {
            continue;
        }
        out.push((format!("{}:{}", name, idx + 1), line));
    }
    Ok(())
}

pub fn run(args: &SizeArgs) -> Result<Outcome> {
    let raws = read_raw_inputs(&args.files, args.lines)?;

    // Parse everything up front so invalid input fails before any output,
    // and an empty record set suppresses the header (exit 1, like `table`).
    let mut docs = Vec::with_capacity(raws.len());
    for (name, raw) in &raws {
        let value: Value =
            serde_json::from_str(raw).with_context(|| format!("invalid JSON: {}", name))?;
        docs.push((name, raw.len(), stats_of(&value)));
    }
    if docs.is_empty() {
        return Ok(Outcome::NotFound);
    }

    let stdout = io::stdout();
    let handle = stdout.lock();
    let mut writer = BoundedWriter::new(handle, args.limit);
    writer.write_decoration("file\tbytes\tdepth\tleaves\tmax_array_len\tkeys")?;
    for (name, bytes, (depth, leaves, max_array, keys)) in &docs {
        if !writer.write_line(&format!(
            "{name}\t{bytes}\t{depth}\t{leaves}\t{max_array}\t{keys}"
        ))? {
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

    #[test]
    fn stats_of_scalar_is_depth_zero_single_leaf() {
        assert_eq!(stats_of(&json!(1)), (0, 1, 0, 0));
        assert_eq!(stats_of(&json!("s")), (0, 1, 0, 0));
        assert_eq!(stats_of(&json!(null)), (0, 1, 0, 0));
    }

    #[test]
    fn stats_of_nested_document() {
        // {"a":1,"b":{"c":[1,2]}}: depth 3, leaves 3, longest array 2, keys 3.
        let v = json!({"a": 1, "b": {"c": [1, 2]}});
        assert_eq!(stats_of(&v), (3, 3, 2, 3));
    }

    #[test]
    fn stats_of_empty_containers() {
        // Empty containers add a level but no leaves, keys, or length.
        assert_eq!(stats_of(&json!([])), (1, 0, 0, 0));
        assert_eq!(stats_of(&json!({})), (1, 0, 0, 0));
        // No arrays anywhere: max_array_len stays 0.
        assert_eq!(stats_of(&json!({"a": {"b": 1}})), (2, 1, 0, 2));
    }

    #[test]
    fn stats_of_array_root_tracks_longest() {
        let v = json!([[1, 2, 3], [4]]);
        assert_eq!(stats_of(&v), (2, 4, 3, 0));
    }

    #[test]
    fn run_simple() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.json");
        std::fs::write(&p, r#"{"a":1}"#).unwrap();
        let args = SizeArgs {
            files: vec![p],
            lines: false,
            limit: None,
        };
        assert_eq!(run(&args).unwrap(), Outcome::Found);
    }

    #[test]
    fn run_lines_mode() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.ndjson");
        std::fs::write(&p, "{\"a\":1}\n{\"b\":[1,2]}\n\n").unwrap();
        let args = SizeArgs {
            files: vec![p],
            lines: true,
            limit: None,
        };
        assert_eq!(run(&args).unwrap(), Outcome::Found);
    }

    #[test]
    fn run_blank_lines_input_is_no_results() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.ndjson");
        std::fs::write(&p, "\n   \n").unwrap();
        let args = SizeArgs {
            files: vec![p],
            lines: true,
            limit: None,
        };
        assert_eq!(run(&args).unwrap(), Outcome::NotFound);
    }

    #[test]
    fn run_invalid_json_is_tool_error() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.json");
        std::fs::write(&p, r#"{"a":}"#).unwrap();
        let args = SizeArgs {
            files: vec![p],
            lines: false,
            limit: None,
        };
        assert!(run(&args).is_err());
    }
}
