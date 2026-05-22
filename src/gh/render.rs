//! Shared output helpers for the `gh` list-style commands.
//!
//! Every `sak gh *-list` command follows the same shape: it runs a
//! `gh <noun> list --json <fields>` invocation (the chokepoint forwards
//! the field list verbatim — sak never invents its own field set), gets
//! back a JSON array of objects, and emits either that JSON unchanged or
//! a TSV projection of the requested fields. The projection logic and
//! the `--format` flag live here so `pr-list`, `issue-list`,
//! `run-list`, and `release-list` stay byte-for-byte consistent.

use anyhow::{Context, Result};
use clap::ValueEnum;
use serde_json::Value;

use crate::output::{BoundedWriter, collapse_ws};

/// Output format shared by every `gh` list command.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum Format {
    /// Tab-separated, one row per record, columns named by `--fields`.
    Tsv,
    /// The JSON array returned by `gh` verbatim.
    Json,
}

/// Split a comma-separated `--fields` value into trimmed, non-empty field
/// names. `"number, title ,,author"` → `["number", "title", "author"]`.
pub fn parse_fields(spec: &str) -> Vec<String> {
    spec.split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

/// Render one scalar-ish JSON value to a cell string, or `None` if the
/// value is a nested array/object that has no obvious atomic projection.
///
/// GitHub's `--json` output mixes scalars with a few well-known object
/// shapes: user objects carry `login`, many named objects carry `name`.
/// We special-case those two keys because they're the natural column
/// value (e.g. `author` → the login), and fall back to `None` for
/// anything genuinely structured so the caller can emit compact JSON.
fn cell_atom(v: &Value) -> Option<String> {
    match v {
        Value::Null => Some("-".to_string()),
        Value::String(s) => Some(collapse_ws(s)),
        Value::Bool(b) => Some(b.to_string()),
        Value::Number(n) => Some(n.to_string()),
        Value::Object(map) => map
            .get("login")
            .and_then(Value::as_str)
            .or_else(|| map.get("name").and_then(Value::as_str))
            .map(collapse_ws),
        Value::Array(_) => None,
    }
}

/// Render a single TSV cell for `field` of `record`.
///
/// - missing / null → `-`
/// - scalar → the scalar
/// - user/named object → its `login` / `name`
/// - array of atoms → comma-joined (`labels` → `bug,enhancement`)
/// - anything else → compact JSON (honest, if ugly; use `--format json`
///   for the full structure)
pub fn render_cell(record: &Value, field: &str) -> String {
    let Some(v) = record.get(field) else {
        return "-".to_string();
    };
    if let Value::Array(arr) = v {
        if arr.is_empty() {
            return "-".to_string();
        }
        let atoms: Option<Vec<String>> = arr.iter().map(cell_atom).collect();
        return match atoms {
            Some(parts) => parts.join(","),
            None => collapse_ws(&serde_json::to_string(v).unwrap_or_default()),
        };
    }
    cell_atom(v).unwrap_or_else(|| collapse_ws(&serde_json::to_string(v).unwrap_or_default()))
}

/// Emit `gh`'s JSON output as either a TSV projection or verbatim JSON.
///
/// `stdout` is the raw bytes from the `gh ... --json` call. For `Tsv`,
/// the header row (`--fields` names) is written as decoration (not
/// counted toward `--limit`); each record becomes one bounded line. For
/// `Json`, the body is streamed through the bounded writer line by line.
///
/// Returns the process exit code: `SUCCESS` when at least one record was
/// present, `1` when the result set was empty (sak's "no results"
/// convention).
pub fn emit(
    writer: &mut BoundedWriter<'_>,
    stdout: &[u8],
    fields: &[String],
    format: Format,
) -> Result<bool> {
    let text = String::from_utf8_lossy(stdout);

    match format {
        Format::Json => write_json_verbatim(writer, &text, "[]"),
        Format::Tsv => {
            let records: Vec<Value> =
                serde_json::from_str(text.trim()).context("parsing `gh ... --json` output")?;
            if records.is_empty() {
                return Ok(false);
            }
            writer.write_decoration(&fields.join("\t"))?;
            for record in &records {
                let row: Vec<String> = fields.iter().map(|f| render_cell(record, f)).collect();
                if !writer.write_line(&row.join("\t"))? {
                    break;
                }
            }
            Ok(true)
        }
    }
}

/// Emit a single `gh ... view --json` object as either verbatim JSON or, for
/// `Tsv`, one `field<TAB>value` line per requested field (the single-record
/// shape isn't a table, so there's no header row and each field is its own
/// line). Used by the `gh *-view` commands.
///
/// Returns `true` when a non-empty object was emitted, `false` for an empty
/// document (`{}` / empty string) so callers can map to sak's exit code 1.
pub fn emit_single(
    writer: &mut BoundedWriter<'_>,
    stdout: &[u8],
    fields: &[String],
    format: Format,
) -> Result<bool> {
    let text = String::from_utf8_lossy(stdout);

    match format {
        Format::Json => write_json_verbatim(writer, &text, "{}"),
        Format::Tsv => {
            let record: Value =
                serde_json::from_str(text.trim()).context("parsing `gh ... --json` output")?;
            let is_empty = match &record {
                Value::Object(map) => map.is_empty(),
                Value::Null => true,
                _ => false,
            };
            if is_empty {
                return Ok(false);
            }
            for field in fields {
                let line = format!("{}\t{}", field, render_cell(&record, field));
                if !writer.write_line(&line)? {
                    break;
                }
            }
            Ok(true)
        }
    }
}

/// Stream a JSON body to the writer unchanged, line by line. `empty_marker`
/// is the trimmed body that counts as "no results" (`[]` for arrays, `{}` for
/// single objects). Shared by [`emit`] and [`emit_single`].
fn write_json_verbatim(
    writer: &mut BoundedWriter<'_>,
    text: &str,
    empty_marker: &str,
) -> Result<bool> {
    let trimmed = text.trim();
    if trimmed.is_empty() || trimmed == empty_marker {
        return Ok(false);
    }
    for line in text.split_inclusive('\n') {
        let line = line.strip_suffix('\n').unwrap_or(line);
        if !writer.write_line(line)? {
            break;
        }
    }
    Ok(true)
}

/// Convenience for commands: lock stdout, build a bounded writer, emit, and
/// flush, mapping the present/empty result to sak's 0/1 exit codes.
pub fn emit_to_stdout(
    stdout: &[u8],
    fields: &[String],
    format: Format,
    limit: Option<usize>,
) -> Result<std::process::ExitCode> {
    finish(emit, stdout, fields, format, limit)
}

/// Single-record counterpart of [`emit_to_stdout`] for the `gh *-view`
/// commands.
pub fn emit_single_to_stdout(
    stdout: &[u8],
    fields: &[String],
    format: Format,
    limit: Option<usize>,
) -> Result<std::process::ExitCode> {
    finish(emit_single, stdout, fields, format, limit)
}

/// Shared stdout-locking / flush / exit-code wrapper for the two emit fns.
fn finish(
    emit_fn: impl FnOnce(&mut BoundedWriter<'_>, &[u8], &[String], Format) -> Result<bool>,
    stdout: &[u8],
    fields: &[String],
    format: Format,
    limit: Option<usize>,
) -> Result<std::process::ExitCode> {
    let out = std::io::stdout();
    let handle = out.lock();
    let mut writer = BoundedWriter::new(handle, limit);
    let any = emit_fn(&mut writer, stdout, fields, format)?;
    writer.flush()?;
    Ok(if any {
        std::process::ExitCode::SUCCESS
    } else {
        std::process::ExitCode::from(1)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parse_fields_trims_and_drops_empty() {
        assert_eq!(
            parse_fields("number, title ,,author"),
            vec!["number", "title", "author"]
        );
        assert!(parse_fields("").is_empty());
        assert!(parse_fields(" , , ").is_empty());
    }

    #[test]
    fn render_scalars() {
        let r = json!({"number": 42, "title": "fix bug", "draft": true});
        assert_eq!(render_cell(&r, "number"), "42");
        assert_eq!(render_cell(&r, "title"), "fix bug");
        assert_eq!(render_cell(&r, "draft"), "true");
    }

    #[test]
    fn render_missing_and_null_as_dash() {
        let r = json!({"a": null});
        assert_eq!(render_cell(&r, "a"), "-");
        assert_eq!(render_cell(&r, "missing"), "-");
    }

    #[test]
    fn render_user_object_uses_login() {
        let r = json!({"author": {"login": "octocat", "name": "The Octocat"}});
        assert_eq!(render_cell(&r, "author"), "octocat");
    }

    #[test]
    fn render_named_object_uses_name() {
        let r = json!({"milestone": {"title": "v1", "name": "Milestone One"}});
        assert_eq!(render_cell(&r, "milestone"), "Milestone One");
    }

    #[test]
    fn render_label_array_joins_names() {
        let r = json!({"labels": [{"name": "bug"}, {"name": "p1"}]});
        assert_eq!(render_cell(&r, "labels"), "bug,p1");
    }

    #[test]
    fn render_empty_array_as_dash() {
        let r = json!({"labels": []});
        assert_eq!(render_cell(&r, "labels"), "-");
    }

    #[test]
    fn render_scalar_array_joins_values() {
        let r = json!({"tags": ["a", "b", "c"]});
        assert_eq!(render_cell(&r, "tags"), "a,b,c");
    }

    #[test]
    fn render_opaque_object_falls_back_to_json() {
        let r = json!({"weird": {"x": 1, "y": 2}});
        let cell = render_cell(&r, "weird");
        assert!(cell.contains("\"x\":1"), "got {cell}");
        assert!(!cell.contains('\t'));
    }

    #[test]
    fn render_collapses_tabs_and_newlines() {
        let r = json!({"title": "line1\nline2\twith tab"});
        let cell = render_cell(&r, "title");
        assert!(!cell.contains('\n') && !cell.contains('\t'), "got {cell:?}");
        assert_eq!(cell, "line1 line2 with tab");
    }
}
