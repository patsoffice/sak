use crate::output::Outcome;
use std::io;
use std::path::PathBuf;

use anyhow::Result;
use clap::Args;
use serde_json::Value;

use crate::json::{collect_path_values, read_json_inputs_maybe_lines, sort::value_cmp};
use crate::output::BoundedWriter;

#[derive(Args)]
#[command(
    about = "Emit distinct values at a path across an array",
    long_about = "Emit the distinct values at <path> (default `.`) across \
        every element of the input array(s) — `sort | uniq` for JSON. \
        Values are sorted for determinism (same order as `json sort`) and \
        emitted as one JSON array. Array results at the path flatten; \
        records where the path is missing are skipped, so a path that \
        resolves nowhere is exit 1, not an empty array.",
    after_help = "\
Examples:
  sak json uniq .id .issues data.json       Distinct ids
  echo '[{\"l\":\"a\"},{\"l\":\"b\"}]' | sak json uniq .l -   Distinct piped values"
)]
pub struct UniqArgs {
    /// Path to the values (a lone existing file is read as input, path `.`)
    pub path: Option<String>,

    /// Input files (reads stdin if omitted or given as `-`)
    pub files: Vec<PathBuf>,

    /// Parse input as NDJSON (one JSON value per line)
    #[arg(long)]
    pub lines: bool,
}

pub fn run(args: &UniqArgs) -> Result<Outcome> {
    // A lone existing file in the path slot is the input (path `.`) —
    // mirroring `slice`, `sort`, and `table`.
    let (path, files) = crate::input::path_or_file(args.path.as_deref(), &args.files);
    let array_path = path.as_deref().unwrap_or(".");
    let inputs = read_json_inputs_maybe_lines(&files, args.lines)?;
    let mut values = collect_path_values(&inputs, array_path)?;
    if values.is_empty() {
        return Ok(Outcome::NotFound);
    }

    // sort | uniq: order by value, then drop adjacent duplicates. Dedupe
    // identity is the canonical JSON text, so 1 and 1.0 stay distinct.
    values.sort_by(|a, b| value_cmp(a, b, false));
    let mut distinct: Vec<Value> = Vec::with_capacity(values.len());
    for v in values {
        let text = serde_json::to_string(&v).unwrap_or_default();
        let keep = distinct
            .last()
            .map(|prev| serde_json::to_string(prev).unwrap_or_default() != text)
            .unwrap_or(true);
        if keep {
            distinct.push(v);
        }
    }

    let stdout = io::stdout();
    let handle = stdout.lock();
    let mut writer = BoundedWriter::new(handle, None);
    writer.write_line(&serde_json::to_string(&Value::Array(distinct)).unwrap_or_default())?;
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
    fn collect_flattens_and_skips_missing() {
        let inputs = named(vec![
            json!({"t": ["x", "y", "x"]}),
            json!({"other": 1}),
            json!({"t": "z"}),
        ]);
        // "y", "x" (dup), "x", then "z"; missing record skipped.
        assert_eq!(
            collect_path_values(&inputs, ".t").unwrap(),
            vec![json!("x"), json!("y"), json!("x"), json!("z")]
        );
        assert!(collect_path_values(&inputs, ".nope").unwrap().is_empty());
    }

    #[test]
    fn collect_treats_array_roots_as_records() {
        let inputs = named(vec![json!([{"s": 1}, {"s": 1}])]);
        assert_eq!(
            collect_path_values(&inputs, ".s").unwrap(),
            vec![json!(1), json!(1)]
        );
    }

    fn write_tmp(content: &str) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.json");
        std::fs::write(&p, content).unwrap();
        (dir, p)
    }

    fn uniq_args(p: PathBuf, lines: bool) -> UniqArgs {
        UniqArgs {
            path: None,
            files: vec![p],
            lines,
        }
    }

    #[test]
    fn run_dedupes() {
        let (_d, p) = write_tmp(r#"[{"l":"b"},{"l":"a"},{"l":"b"}]"#);
        let args = UniqArgs {
            path: Some(".l".to_string()),
            files: vec![p],
            lines: false,
        };
        assert_eq!(run(&args).unwrap(), Outcome::Found);
    }

    #[test]
    fn run_all_missing_is_no_results() {
        let (_d, p) = write_tmp(r#"[{"a":1}]"#);
        let args = UniqArgs {
            path: Some(".nope".to_string()),
            files: vec![p],
            lines: false,
        };
        assert_eq!(run(&args).unwrap(), Outcome::NotFound);
    }

    #[test]
    fn run_lines_mode() {
        let (_d, p) = write_tmp("{\"l\":\"a\"}\n{\"l\":\"a\"}\n");
        assert_eq!(run(&uniq_args(p, true)).unwrap(), Outcome::Found);
    }

    #[test]
    fn run_lone_file_positional_is_input() {
        let (_d, p) = write_tmp("[1,1,2]");
        let args = UniqArgs {
            path: Some(p.display().to_string()),
            files: vec![],
            lines: false,
        };
        assert_eq!(run(&args).unwrap(), Outcome::Found);
    }
}
