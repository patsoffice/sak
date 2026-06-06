use crate::output::Outcome;
use std::io;
use std::path::PathBuf;

use anyhow::{Result, bail};
use clap::Args;
use serde_json::Value;

use crate::json::read_json_inputs_maybe_lines;
use crate::output::{BoundedWriter, collapse_ws};
use crate::value::{format_value, resolve_expression, type_name};

#[derive(Args)]
#[command(
    about = "Project an array of objects to a TSV table (one row per element)",
    long_about = "Iterate a JSON array of objects at a path and project N named \
        fields from each element into a tab-separated row — the `jq -r '.[] | \
        [.a, .b] | @tsv'` shape, without the jq.\n\n\
        `<array-path>` is a dot path (`.issues`) or JSON Pointer (`/issues`) \
        pointing at the array; it defaults to `.` (the document root must then \
        be an array). The root resolving to anything other than an array is an \
        error.\n\n\
        Each `--fields` entry is itself a dot path or JSON Pointer resolved \
        against every element. A field missing from an element (or resolving to \
        null) renders as the `--missing` token (default `-`); the same token \
        fills a cell for a scalar (non-object) array element, except the first \
        column, which gets the scalar itself. Tabs and newlines inside a \
        projected value are collapsed to spaces so a value can't break the \
        one-row-per-element TSV contract.\n\n\
        With `--lines`, the input is read as NDJSON (one JSON value per line) \
        rather than one wrapped document; `<array-path>` is resolved against \
        each line and the result becomes that line's row. With the default `.` \
        path each line is itself a row. Multiple input files are concatenated.\n\n\
        Exit codes follow sak convention: 0 = at least one row emitted, 1 = the \
        array was empty (no rows), 2 = error (bad path, non-array, parse error).",
    after_help = "\
Examples:
  br list --json | sak json table .issues --fields id,priority,title
  sak json table --fields name,version pkgs.json        Root is an array of objects
  sak json table .items -f id,user.name,ts data.json    Nested field paths
  sak json table .rows -f a,b --header data.json         Emit a header row
  sak json table .rows -f a,b --missing NULL data.json   Custom missing token
  cat events.ndjson | sak json table --lines -f level,msg
                                                        NDJSON: each line is a row"
)]
pub struct TableArgs {
    /// Path to the array of objects (dot notation or JSON Pointer; default `.`)
    #[arg(default_value = ".")]
    pub array_path: String,

    /// Input files (reads stdin if omitted or given as "-")
    pub files: Vec<PathBuf>,

    /// Comma-separated field paths to project from each element (required)
    #[arg(short = 'f', long, required = true)]
    pub fields: String,

    /// Emit a header row (the field paths, tab-separated)
    #[arg(long)]
    pub header: bool,

    /// Token substituted for missing/null fields
    #[arg(long, default_value = "-")]
    pub missing: String,

    /// Parse input as NDJSON (one JSON value per line)
    #[arg(long)]
    pub lines: bool,

    /// Maximum number of output lines
    #[arg(long)]
    pub limit: Option<usize>,
}

/// Parse the comma-separated `--fields` argument into trimmed field paths. Each
/// field doubles as its own header. Empty entries are rejected.
fn parse_fields(spec: &str) -> Result<Vec<String>> {
    let mut out = Vec::new();
    for raw in spec.split(',') {
        let part = raw.trim();
        if part.is_empty() {
            bail!("empty field in --fields spec");
        }
        out.push(part.to_string());
    }
    if out.is_empty() {
        bail!("at least one field is required");
    }
    Ok(out)
}

/// Render one resolved value to a TSV-safe cell: missing path or JSON null
/// becomes `missing`; scalars render raw; arrays/objects render as compact JSON.
/// Tabs and newlines are collapsed so a value can't break the TSV row.
fn render_cell(value: Option<&Value>, missing: &str) -> String {
    match value {
        None | Some(Value::Null) => missing.to_string(),
        Some(v) => collapse_ws(&format_value(v, true, false)),
    }
}

/// Project one array element into a row of TSV cells.
///
/// Object (and array) elements resolve each field path independently. A scalar
/// element (string/number/bool/null) has no addressable fields, so it renders
/// in the first column with the `missing` token filling the rest — preserving
/// the column count for ragged scalar/object mixes.
fn project_row(element: &Value, fields: &[String], missing: &str) -> Result<Vec<String>> {
    if !element.is_object() && !element.is_array() {
        let mut row = Vec::with_capacity(fields.len());
        row.push(render_cell(Some(element), missing));
        row.extend(std::iter::repeat_n(missing.to_string(), fields.len() - 1));
        return Ok(row);
    }
    fields
        .iter()
        .map(|f| Ok(render_cell(resolve_expression(element, f)?, missing)))
        .collect()
}

pub fn run(args: &TableArgs) -> Result<Outcome> {
    let fields = parse_fields(&args.fields)?;
    let inputs = read_json_inputs_maybe_lines(&args.files, args.lines)?;

    // Gather every row source up front (refs into `inputs`, which outlives this
    // loop) so an empty result can suppress the header, matching `k8s get
    // --columns`. In --lines mode each parsed line resolves to one element; in
    // whole-document mode the array-path must land on an array.
    let mut elements: Vec<&Value> = Vec::new();
    for (name, value) in &inputs {
        if args.lines {
            match resolve_expression(value, &args.array_path)? {
                Some(v) => elements.push(v),
                None => bail!("path {:?} did not resolve in {}", args.array_path, name),
            }
        } else {
            match resolve_expression(value, &args.array_path)? {
                Some(Value::Array(items)) => elements.extend(items.iter()),
                Some(other) => bail!(
                    "expected a JSON array at {:?} in {}, found {}",
                    args.array_path,
                    name,
                    type_name(other)
                ),
                None => bail!("path {:?} did not resolve in {}", args.array_path, name),
            }
        }
    }

    let stdout = io::stdout();
    let handle = stdout.lock();
    let mut writer = BoundedWriter::new(handle, args.limit);

    if elements.is_empty() {
        writer.flush()?;
        return Ok(Outcome::NotFound);
    }

    if args.header {
        writer.write_decoration(&fields.join("\t"))?;
    }

    let mut wrote_any = false;
    for element in elements {
        let row = project_row(element, &fields, &args.missing)?;
        if !writer.write_line(&row.join("\t"))? {
            break;
        }
        wrote_any = true;
    }
    writer.flush()?;
    Ok(if wrote_any {
        Outcome::Found
    } else {
        Outcome::NotFound
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::io::Write;

    fn write_tmp(content: &str) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.json");
        let mut f = std::fs::File::create(&p).unwrap();
        f.write_all(content.as_bytes()).unwrap();
        (dir, p)
    }

    #[test]
    fn parse_fields_trims_and_rejects_empty() {
        assert_eq!(parse_fields("a, b ,c").unwrap(), vec!["a", "b", "c"]);
        assert!(parse_fields("").is_err());
        assert!(parse_fields("a,,b").is_err());
        assert!(parse_fields("a, ,b").is_err());
    }

    #[test]
    fn project_row_basic_object() {
        let el = json!({"id": 1, "priority": 2, "title": "x"});
        let fields = parse_fields("id,priority,title").unwrap();
        let row = project_row(&el, &fields, "-").unwrap();
        assert_eq!(row, vec!["1", "2", "x"]);
    }

    #[test]
    fn project_row_missing_field_uses_token() {
        let el = json!({"id": 1});
        let fields = parse_fields("id,missing").unwrap();
        assert_eq!(project_row(&el, &fields, "-").unwrap(), vec!["1", "-"]);
        // Custom token honored.
        assert_eq!(
            project_row(&el, &fields, "NULL").unwrap(),
            vec!["1", "NULL"]
        );
    }

    #[test]
    fn project_row_null_field_uses_token() {
        let el = json!({"a": null, "b": 2});
        let fields = parse_fields("a,b").unwrap();
        assert_eq!(project_row(&el, &fields, "-").unwrap(), vec!["-", "2"]);
    }

    #[test]
    fn project_row_nested_paths() {
        let el = json!({"user": {"name": "alice"}, "ts": "2024"});
        let fields = parse_fields("user.name,ts").unwrap();
        assert_eq!(
            project_row(&el, &fields, "-").unwrap(),
            vec!["alice", "2024"]
        );
        // JSON Pointer form too.
        let fields = parse_fields("/user/name").unwrap();
        assert_eq!(project_row(&el, &fields, "-").unwrap(), vec!["alice"]);
    }

    #[test]
    fn project_row_complex_value_is_compact_json() {
        let el = json!({"tags": ["a", "b"], "meta": {"k": "v"}});
        let fields = parse_fields("tags,meta").unwrap();
        assert_eq!(
            project_row(&el, &fields, "-").unwrap(),
            vec![r#"["a","b"]"#, r#"{"k":"v"}"#]
        );
    }

    #[test]
    fn project_row_collapses_whitespace() {
        let el = json!({"msg": "a\tb\nc"});
        let fields = parse_fields("msg").unwrap();
        let row = project_row(&el, &fields, "-").unwrap();
        assert_eq!(row, vec!["a b c"]);
        assert!(!row[0].contains('\t'));
        assert!(!row[0].contains('\n'));
    }

    #[test]
    fn project_row_scalar_element_fills_first_column() {
        // Non-object element: scalar in column 0, missing token elsewhere.
        let fields = parse_fields("a,b,c").unwrap();
        assert_eq!(
            project_row(&json!(42), &fields, "-").unwrap(),
            vec!["42", "-", "-"]
        );
        assert_eq!(
            project_row(&json!("hi"), &fields, "-").unwrap(),
            vec!["hi", "-", "-"]
        );
    }

    #[test]
    fn run_wrapped_array() {
        let (_d, p) = write_tmp(r#"{"issues":[{"id":1,"t":"a"},{"id":2,"t":"b"}]}"#);
        let args = TableArgs {
            array_path: ".issues".to_string(),
            files: vec![p],
            fields: "id,t".to_string(),
            header: false,
            missing: "-".to_string(),
            lines: false,
            limit: None,
        };
        assert_eq!(run(&args).unwrap(), Outcome::Found);
    }

    #[test]
    fn run_root_array_default_path() {
        let (_d, p) = write_tmp(r#"[{"id":1},{"id":2}]"#);
        let args = TableArgs {
            array_path: ".".to_string(),
            files: vec![p],
            fields: "id".to_string(),
            header: false,
            missing: "-".to_string(),
            lines: false,
            limit: None,
        };
        assert_eq!(run(&args).unwrap(), Outcome::Found);
    }

    #[test]
    fn run_empty_array_is_not_found() {
        let (_d, p) = write_tmp(r#"{"issues":[]}"#);
        let args = TableArgs {
            array_path: ".issues".to_string(),
            files: vec![p],
            fields: "id".to_string(),
            header: true,
            missing: "-".to_string(),
            lines: false,
            limit: None,
        };
        // Empty array → no rows, header suppressed, exit 1.
        assert_eq!(run(&args).unwrap(), Outcome::NotFound);
    }

    #[test]
    fn run_non_array_path_is_error() {
        let (_d, p) = write_tmp(r#"{"issues":{"not":"an array"}}"#);
        let args = TableArgs {
            array_path: ".issues".to_string(),
            files: vec![p],
            fields: "id".to_string(),
            header: false,
            missing: "-".to_string(),
            lines: false,
            limit: None,
        };
        let err = run(&args).unwrap_err().to_string();
        assert!(err.contains("expected a JSON array"), "{err}");
    }

    #[test]
    fn run_missing_path_is_error() {
        let (_d, p) = write_tmp(r#"{"issues":[]}"#);
        let args = TableArgs {
            array_path: ".nope".to_string(),
            files: vec![p],
            fields: "id".to_string(),
            header: false,
            missing: "-".to_string(),
            lines: false,
            limit: None,
        };
        let err = run(&args).unwrap_err().to_string();
        assert!(err.contains("did not resolve"), "{err}");
    }

    #[test]
    fn run_lines_mode_each_line_is_a_row() {
        let (_d, p) =
            write_tmp("{\"level\":\"info\",\"msg\":\"a\"}\n{\"level\":\"warn\",\"msg\":\"b\"}\n");
        let args = TableArgs {
            array_path: ".".to_string(),
            files: vec![p],
            fields: "level,msg".to_string(),
            header: false,
            missing: "-".to_string(),
            lines: true,
            limit: None,
        };
        assert_eq!(run(&args).unwrap(), Outcome::Found);
    }
}
