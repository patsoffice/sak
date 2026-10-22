use crate::output::Outcome;
use std::io::{self, Read};
use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::Args;
use serde_json::Value;

use crate::output::BoundedWriter;

#[derive(Args)]
#[command(
    about = "Check whether files contain valid JSON",
    long_about = "Check whether files contain valid JSON and report parse errors.\n\n\
        Reads from stdin if no files are given, or for a file argument of `-`. \
        Exits 0 if all inputs are valid, \
        exits 1 if any input is invalid. Errors are reported to stderr as \
        `filename:line:column: message`.\n\n\
        With `--lines`, each non-blank line of the input is validated independently \
        as NDJSON. Errors are reported per-line as `filename:lineno: message` and \
        validation continues across invalid lines. Blank / whitespace-only lines \
        are skipped.",
    after_help = "\
Examples:
  sak json validate config.json              Validate a single file
  sak json validate *.json                   Validate multiple files
  echo '{\"a\":1}' | sak json validate       Validate piped input
  sak json validate --quiet config.json      Exit code only, no output
  sak json validate --lines events.ndjson    Validate each NDJSON record"
)]
pub struct ValidateArgs {
    /// Input files (reads stdin if omitted or given as "-")
    pub files: Vec<PathBuf>,

    /// Exit code only, no output
    #[arg(short, long)]
    pub quiet: bool,

    /// Reject duplicate object keys (default parsing keeps the last)
    #[arg(long)]
    pub strict: bool,

    /// Parse input as NDJSON (one JSON value per line); errors reported per line
    #[arg(long)]
    pub lines: bool,

    /// Maximum number of output lines
    #[arg(long)]
    pub limit: Option<usize>,
}

fn validate_one(name: &str, content: &str, strict: bool) -> std::result::Result<(), String> {
    if strict {
        match serde_json::from_str::<NoDup>(content) {
            Ok(_) => Ok(()),
            Err(e) => Err(format!("{}:{}:{}: {}", name, e.line(), e.column(), e)),
        }
    } else {
        match serde_json::from_str::<Value>(content) {
            Ok(_) => Ok(()),
            Err(e) => Err(format!("{}:{}:{}: {}", name, e.line(), e.column(), e)),
        }
    }
}

/// `serde_json::Value` that rejects duplicate object keys at any depth.
/// Plain `Value` keeps the last duplicate silently, which is fine for
/// reading but wrong for validating — `--strict` turns it into an error
/// naming the offending key.
struct NoDup(Value);

impl<'de> serde::Deserialize<'de> for NoDup {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        deserializer.deserialize_any(NoDupVisitor)
    }
}

struct NoDupVisitor;

impl<'de> serde::de::Visitor<'de> for NoDupVisitor {
    type Value = NoDup;

    fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
        formatter.write_str("any valid JSON value with unique object keys")
    }

    fn visit_bool<E>(self, v: bool) -> std::result::Result<NoDup, E> {
        Ok(NoDup(Value::Bool(v)))
    }

    fn visit_i64<E>(self, v: i64) -> std::result::Result<NoDup, E> {
        Ok(NoDup(Value::Number(v.into())))
    }

    fn visit_u64<E>(self, v: u64) -> std::result::Result<NoDup, E> {
        Ok(NoDup(Value::Number(v.into())))
    }

    fn visit_f64<E>(self, v: f64) -> std::result::Result<NoDup, E> {
        Ok(NoDup(
            serde_json::Number::from_f64(v).map_or(Value::Null, Value::Number),
        ))
    }

    fn visit_str<E>(self, v: &str) -> std::result::Result<NoDup, E>
    where
        E: serde::de::Error,
    {
        Ok(NoDup(Value::String(v.to_owned())))
    }

    fn visit_string<E>(self, v: String) -> std::result::Result<NoDup, E> {
        Ok(NoDup(Value::String(v)))
    }

    fn visit_none<E>(self) -> std::result::Result<NoDup, E> {
        Ok(NoDup(Value::Null))
    }

    fn visit_unit<E>(self) -> std::result::Result<NoDup, E> {
        Ok(NoDup(Value::Null))
    }

    fn visit_seq<A>(self, mut seq: A) -> std::result::Result<NoDup, A::Error>
    where
        A: serde::de::SeqAccess<'de>,
    {
        let mut items = Vec::with_capacity(seq.size_hint().unwrap_or(0));
        while let Some(item) = seq.next_element::<NoDup>()? {
            items.push(item.0);
        }
        Ok(NoDup(Value::Array(items)))
    }

    fn visit_map<A>(self, mut map: A) -> std::result::Result<NoDup, A::Error>
    where
        A: serde::de::MapAccess<'de>,
    {
        let mut seen = std::collections::HashSet::new();
        let mut obj = serde_json::Map::with_capacity(map.size_hint().unwrap_or(0));
        while let Some(key) = map.next_key::<String>()? {
            if !seen.insert(key.clone()) {
                return Err(serde::de::Error::custom(format!(
                    "duplicate object key {key:?}"
                )));
            }
            obj.insert(key, map.next_value::<NoDup>()?.0);
        }
        Ok(NoDup(Value::Object(obj)))
    }
}

/// Validate `content` as NDJSON. Returns (total_non_blank_lines, error_messages).
/// Each error message is prefixed with `name:lineno:`.
fn validate_lines(name: &str, content: &str, strict: bool) -> (usize, Vec<String>) {
    let mut total = 0;
    let mut errs = Vec::new();
    for (idx, line) in content.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        total += 1;
        let lineno = idx + 1;
        let parsed = if strict {
            serde_json::from_str::<NoDup>(line).map(|_| ())
        } else {
            serde_json::from_str::<Value>(line).map(|_| ())
        };
        if let Err(e) = parsed {
            errs.push(format!("{}:{}: {}", name, lineno, e));
        }
    }
    (total, errs)
}

pub fn run(args: &ValidateArgs) -> Result<Outcome> {
    let _ = args.strict; // reserved
    let stdout = io::stdout();
    let handle = stdout.lock();
    let mut writer = BoundedWriter::new(handle, args.limit);

    let mut any_invalid = false;

    let mut sources: Vec<(String, String)> = Vec::new();
    if args.files.is_empty() {
        crate::input::require_piped_stdin()?;
        let mut s = String::new();
        io::stdin()
            .read_to_string(&mut s)
            .context("error reading stdin")?;
        sources.push(("<stdin>".to_string(), s));
    } else {
        for path in &args.files {
            if crate::json::is_stdin(path) {
                let mut s = String::new();
                io::stdin()
                    .read_to_string(&mut s)
                    .context("error reading stdin")?;
                sources.push(("<stdin>".to_string(), s));
                continue;
            }
            let name = path.display().to_string();
            match std::fs::read_to_string(path) {
                Ok(s) => sources.push((name, s)),
                Err(e) => {
                    any_invalid = true;
                    if !args.quiet {
                        eprintln!("{}: cannot read: {}", name, e);
                    }
                }
            }
        }
    }

    for (name, content) in &sources {
        if args.lines {
            let (total, errs) = validate_lines(name, content, args.strict);
            if !errs.is_empty() {
                any_invalid = true;
                if !args.quiet {
                    for msg in &errs {
                        eprintln!("{}", msg);
                    }
                }
            }
            if !args.quiet && errs.is_empty() {
                let line = format!("{}: valid ({} records)", name, total);
                if !writer.write_line(&line)? {
                    break;
                }
            }
        } else {
            match validate_one(name, content, args.strict) {
                Ok(()) => {
                    if !args.quiet && !writer.write_line(&format!("{}: valid", name))? {
                        break;
                    }
                }
                Err(msg) => {
                    any_invalid = true;
                    if !args.quiet {
                        eprintln!("{}", msg);
                    }
                }
            }
        }
    }

    writer.flush()?;
    if any_invalid {
        Ok(Outcome::NotFound)
    } else {
        Ok(Outcome::Found)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn valid_file() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.json");
        let mut f = std::fs::File::create(&p).unwrap();
        writeln!(f, r#"{{"a": 1}}"#).unwrap();
        let args = ValidateArgs {
            files: vec![p],
            quiet: true,
            strict: false,
            lines: false,
            limit: None,
        };
        let exit = run(&args).unwrap();
        assert_eq!(exit, Outcome::Found);
    }

    #[test]
    fn invalid_file() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("bad.json");
        let mut f = std::fs::File::create(&p).unwrap();
        writeln!(f, "{{not json").unwrap();
        let args = ValidateArgs {
            files: vec![p],
            quiet: true,
            strict: false,
            lines: false,
            limit: None,
        };
        let exit = run(&args).unwrap();
        assert_eq!(exit, Outcome::NotFound);
    }

    #[test]
    fn validate_one_reports_position() {
        let err = validate_one("x.json", "{\n  bad", false).unwrap_err();
        assert!(err.starts_with("x.json:"));
    }

    #[test]
    fn validate_lines_counts_records_and_skips_blanks() {
        let (total, errs) = validate_lines(
            "f.ndjson",
            "{\"a\":1}\n\n{\"b\":2}\n   \n{\"c\":3}\n",
            false,
        );
        assert_eq!(total, 3);
        assert!(errs.is_empty());
    }

    #[test]
    fn validate_lines_reports_bad_records_with_lineno() {
        let (total, errs) = validate_lines("f.ndjson", "{\"a\":1}\nnot json\n{\"b\":2}\n", false);
        assert_eq!(total, 3);
        assert_eq!(errs.len(), 1);
        assert!(errs[0].starts_with("f.ndjson:2:"));
    }

    #[test]
    fn default_keeps_last_duplicate_silently() {
        // Documents the default: serde_json takes the last duplicate with no
        // error, which is exactly what --strict rejects.
        assert!(validate_one("d.json", "{\"a\":1,\"a\":2}", false).is_ok());
    }

    #[test]
    fn strict_rejects_top_level_duplicates() {
        let err = validate_one("d.json", "{\"a\":1,\"a\":2}", true).unwrap_err();
        assert!(err.contains("duplicate object key"), "{err}");
        assert!(err.starts_with("d.json:"), "{err}");
    }

    #[test]
    fn strict_rejects_nested_duplicates() {
        let err = validate_one("d.json", "{\"a\":{\"b\":1,\"b\":2}}", true).unwrap_err();
        assert!(err.contains("\"b\""), "{err}");
    }

    #[test]
    fn strict_rejects_duplicates_in_arrays() {
        let err = validate_one("d.json", "[{\"a\":1},{\"a\":1,\"a\":2}]", true).unwrap_err();
        assert!(err.contains("duplicate object key"), "{err}");
    }

    #[test]
    fn strict_accepts_clean_documents() {
        assert!(validate_one("d.json", "{\"a\":1,\"b\":[1,{\"c\":null}]}", true).is_ok());
    }

    #[test]
    fn strict_lines_mode_rejects_dup_records() {
        let (total, errs) = validate_lines("f.ndjson", "{\"a\":1}\n{\"a\":1,\"a\":2}\n", true);
        assert_eq!(total, 2);
        assert_eq!(errs.len(), 1);
        assert!(errs[0].starts_with("f.ndjson:2:"), "{}", errs[0]);
    }

    #[test]
    fn lines_mode_all_valid_succeeds() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.ndjson");
        let mut f = std::fs::File::create(&p).unwrap();
        f.write_all(b"{\"a\":1}\n{\"a\":2}\n").unwrap();
        let args = ValidateArgs {
            files: vec![p],
            quiet: true,
            strict: false,
            lines: true,
            limit: None,
        };
        assert_eq!(run(&args).unwrap(), Outcome::Found);
    }

    #[test]
    fn lines_mode_one_bad_record_fails() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("bad.ndjson");
        let mut f = std::fs::File::create(&p).unwrap();
        f.write_all(b"{\"a\":1}\nnope\n{\"a\":2}\n").unwrap();
        let args = ValidateArgs {
            files: vec![p],
            quiet: true,
            strict: false,
            lines: true,
            limit: None,
        };
        assert_eq!(run(&args).unwrap(), Outcome::NotFound);
    }
}
