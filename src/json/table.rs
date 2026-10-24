use crate::output::Outcome;
use std::io;
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use clap::Args;
use serde_json::Value;

use crate::json::read_json_inputs_maybe_lines;
use crate::output::{BoundedWriter, collapse_ws};
use crate::value::{format_value, resolve_expression, type_name};
use regex::Regex;

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
        With `--where '<path> <op> <literal>'`, only elements matching every \
        predicate earn a row; the flag repeats and predicates AND together. \
        Operators are `=`, `!=`, `~` (regex), `!~`, `<`, `<=`, `>`, `>=`; the \
        literal is a JSON string (double-quoted), number, `true`, `false`, or \
        `null`. Strings compare as text and numbers numerically (`3` matches \
        `3.0`); mixed types never order. A regex runs against a string's own \
        text and any other value's compact JSON form. A path that resolves to \
        nothing never equals (`=` false, `!=` true) and never orders. \
        Filtering everything out is exit 1, like an empty array.\n\n\
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
                                                        NDJSON: each line is a row
  sak json table .issues -f id,title --where '.priority < 3'
  sak json table .issues -f id,title --where '.status = \"open\"'"
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

    /// Keep only elements matching `<path> <op> <literal>`; repeatable, ANDed
    #[arg(long = "where", value_name = "EXPR")]
    pub where_exprs: Vec<String>,
}

/// Parse the comma-separated `--fields` argument into trimmed field paths. Each
/// field doubles as its own header. Empty entries are rejected. Shared with
/// `config table`.
pub(crate) fn parse_fields(spec: &str) -> Result<Vec<String>> {
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
/// Shared with `config table`.
pub(crate) fn render_cell(value: Option<&Value>, missing: &str) -> String {
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
/// the column count for ragged scalar/object mixes. Shared with `config table`.
pub(crate) fn project_row(
    element: &Value,
    fields: &[String],
    missing: &str,
) -> Result<Vec<String>> {
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

/// Comparison operator in a `--where` predicate.
#[derive(Debug, PartialEq)]
pub(crate) enum WhereOp {
    Eq,
    NotEq,
    Match,
    NotMatch,
    Lt,
    Lte,
    Gt,
    Gte,
}

/// One parsed `--where '<path> <op> <literal>'` predicate. The regex is
/// compiled at parse time so a bad pattern is an argument error, not a
/// per-row failure.
#[derive(Debug)]
pub(crate) struct WherePred {
    path: String,
    op: WhereOp,
    literal: Value,
    regex: Option<Regex>,
}

/// Split a predicate into path, operator, and raw literal. The operator is
/// the first `=`, `!=`, `~`, `!~`, `<`, `<=`, `>`, `>=` outside a
/// double-quoted string, so literals like `"a=b"` stay intact.
fn split_where(spec: &str) -> Result<(&str, WhereOp, &str)> {
    let bytes = spec.as_bytes();
    let mut in_str = false;
    let mut escaped = false;
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if in_str {
            if escaped {
                escaped = false;
            } else if b == b'\\' {
                escaped = true;
            } else if b == b'"' {
                in_str = false;
            }
            i += 1;
            continue;
        }
        if b == b'"' {
            in_str = true;
            i += 1;
            continue;
        }
        if i + 1 < bytes.len() {
            let two = match &bytes[i..i + 2] {
                b"!=" => Some(WhereOp::NotEq),
                b"!~" => Some(WhereOp::NotMatch),
                b"<=" => Some(WhereOp::Lte),
                b">=" => Some(WhereOp::Gte),
                _ => None,
            };
            if let Some(op) = two {
                return split_at(spec, i, 2, op);
            }
        }
        let one = match b {
            b'=' => Some(WhereOp::Eq),
            b'~' => Some(WhereOp::Match),
            b'<' => Some(WhereOp::Lt),
            b'>' => Some(WhereOp::Gt),
            _ => None,
        };
        if let Some(op) = one {
            return split_at(spec, i, 1, op);
        }
        i += 1;
    }
    bail!("invalid --where {spec:?}: expected `<path> <op> <literal>`")
}

fn split_at(spec: &str, at: usize, width: usize, op: WhereOp) -> Result<(&str, WhereOp, &str)> {
    // Split points are ASCII operator bytes, hence char boundaries.
    let path = spec[..at].trim();
    let raw = spec[at + width..].trim();
    if path.is_empty() || raw.is_empty() {
        bail!("invalid --where {spec:?}: expected `<path> <op> <literal>`");
    }
    Ok((path, op, raw))
}

/// Parse one `--where` expression. The literal is a JSON value — a
/// double-quoted string (with the usual escapes), a number, `true`, `false`,
/// or `null` — so `"a=b"` and `"say \"hi\""` parse as written. The same
/// dot-path / JSON-Pointer syntax as `--fields` addresses the element.
pub(crate) fn parse_where(spec: &str) -> Result<WherePred> {
    let (path, op, raw) = split_where(spec)?;
    let literal: Value = serde_json::from_str(raw).with_context(|| {
        format!(
            "invalid literal in --where {spec:?}: expected \"string\", number, true, false, or null"
        )
    })?;
    let regex = match op {
        WhereOp::Match | WhereOp::NotMatch => {
            let pat = literal
                .as_str()
                .with_context(|| format!("invalid --where {spec:?}: `~` needs a string literal"))?;
            Some(Regex::new(pat).with_context(|| format!("invalid regex in --where {spec:?}"))?)
        }
        _ => None,
    };
    Ok(WherePred {
        path: path.to_string(),
        op,
        literal,
        regex,
    })
}

/// Numeric comparison across JSON number representations, so `3` matches
/// `3.0`. Falls back to f64 when the operands fit no common integer type.
fn num_cmp(a: &serde_json::Number, b: &serde_json::Number) -> Option<std::cmp::Ordering> {
    if let (Some(x), Some(y)) = (a.as_i64(), b.as_i64()) {
        return x.partial_cmp(&y);
    }
    if let (Some(x), Some(y)) = (a.as_u64(), b.as_u64()) {
        return x.partial_cmp(&y);
    }
    a.as_f64()?.partial_cmp(&b.as_f64()?)
}

fn eq_value(actual: Option<&Value>, expected: &Value) -> bool {
    match (actual, expected) {
        (Some(Value::Number(a)), Value::Number(b)) => {
            num_cmp(a, b) == Some(std::cmp::Ordering::Equal)
        }
        (Some(v), e) => v == e,
        (None, _) => false,
    }
}

fn ord_value(actual: Option<&Value>, expected: &Value) -> Option<std::cmp::Ordering> {
    match (actual, expected) {
        (Some(Value::Number(a)), Value::Number(b)) => num_cmp(a, b),
        (Some(Value::String(a)), Value::String(b)) => a.partial_cmp(b),
        _ => None,
    }
}

/// Text a `~` match runs against: a string's own text, anything else's
/// compact JSON form (so `1` matches `~ "^1$"`).
fn where_text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// Evaluate a parsed predicate against one array element. A path that
/// doesn't resolve never equals (`=` false, `!=` true) and never orders.
pub(crate) fn eval_where(pred: &WherePred, element: &Value) -> Result<bool> {
    use std::cmp::Ordering;
    let actual = resolve_expression(element, &pred.path)?;
    Ok(match pred.op {
        WhereOp::Eq => eq_value(actual, &pred.literal),
        WhereOp::NotEq => !eq_value(actual, &pred.literal),
        WhereOp::Match => match actual {
            Some(v) => pred
                .regex
                .as_ref()
                .expect("regex compiled at parse")
                .is_match(&where_text(v)),
            None => false,
        },
        WhereOp::NotMatch => match actual {
            Some(v) => !pred
                .regex
                .as_ref()
                .expect("regex compiled at parse")
                .is_match(&where_text(v)),
            None => true,
        },
        WhereOp::Lt => ord_value(actual, &pred.literal) == Some(Ordering::Less),
        WhereOp::Lte => matches!(
            ord_value(actual, &pred.literal),
            Some(Ordering::Less | Ordering::Equal)
        ),
        WhereOp::Gt => ord_value(actual, &pred.literal) == Some(Ordering::Greater),
        WhereOp::Gte => matches!(
            ord_value(actual, &pred.literal),
            Some(Ordering::Greater | Ordering::Equal)
        ),
    })
}

pub fn run(args: &TableArgs) -> Result<Outcome> {
    let fields = parse_fields(&args.fields)?;
    let mut preds = Vec::with_capacity(args.where_exprs.len());
    for spec in &args.where_exprs {
        preds.push(parse_where(spec)?);
    }
    // `sak json table --fields a,b rows.json` puts the file in the
    // array-path slot; a lone existing file there is the input, path `.`.
    let (path, files) = crate::input::path_or_file(Some(&args.array_path), &args.files);
    let array_path = path.unwrap_or_else(|| ".".to_string());
    let inputs = read_json_inputs_maybe_lines(&files, args.lines)?;

    // Gather every row source up front (refs into `inputs`, which outlives this
    // loop) so an empty result can suppress the header, matching `k8s get
    // --columns`. In --lines mode each parsed line resolves to one element; in
    // whole-document mode the array-path must land on an array.
    let mut elements: Vec<&Value> = Vec::new();
    for (name, value) in &inputs {
        if args.lines {
            match resolve_expression(value, &array_path)? {
                Some(v) => elements.push(v),
                None => bail!("path {:?} did not resolve in {}", array_path, name),
            }
        } else {
            match resolve_expression(value, &array_path)? {
                Some(Value::Array(items)) => elements.extend(items.iter()),
                Some(other) => bail!(
                    "expected a JSON array at {:?} in {}, found {}",
                    array_path,
                    name,
                    type_name(other)
                ),
                None => bail!("path {:?} did not resolve in {}", array_path, name),
            }
        }
    }

    // Predicates AND: an element must satisfy every `--where` to earn a row.
    // Filtering before the empty check keeps the exit contract (no rows →
    // exit 1, header suppressed) and the `--limit` accounting unchanged.
    if !preds.is_empty() {
        let mut kept = Vec::with_capacity(elements.len());
        for element in elements {
            let mut ok = true;
            for pred in &preds {
                if !eval_where(pred, element)? {
                    ok = false;
                    break;
                }
            }
            if ok {
                kept.push(element);
            }
        }
        elements = kept;
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
            where_exprs: vec![],
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
            where_exprs: vec![],
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
            where_exprs: vec![],
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
            where_exprs: vec![],
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
            where_exprs: vec![],
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
            where_exprs: vec![],
        };
        assert_eq!(run(&args).unwrap(), Outcome::Found);
    }

    fn where_args(p: PathBuf, wheres: &[&str]) -> TableArgs {
        TableArgs {
            array_path: ".issues".to_string(),
            files: vec![p],
            fields: "id,title".to_string(),
            header: false,
            missing: "-".to_string(),
            lines: false,
            limit: None,
            where_exprs: wheres.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn parse_where_operators() {
        use WhereOp::*;
        for (spec, path, op) in [
            (".a = 1", ".a", Eq),
            (".a != 1", ".a", NotEq),
            (".a ~ \"x\"", ".a", Match),
            (".a !~ \"x\"", ".a", NotMatch),
            (".a < 1", ".a", Lt),
            (".a <= 1", ".a", Lte),
            (".a > 1", ".a", Gt),
            (".a >= 1", ".a", Gte),
            ("/a/b = true", "/a/b", Eq),
        ] {
            let pred = parse_where(spec).unwrap();
            assert_eq!((pred.path.as_str(), pred.op), (path, op), "{spec}");
        }
    }

    #[test]
    fn parse_where_literals() {
        assert_eq!(parse_where(".a = 1").unwrap().literal, json!(1));
        assert_eq!(parse_where(".a = 1.5").unwrap().literal, json!(1.5));
        assert_eq!(parse_where(".a = \"x\"").unwrap().literal, json!("x"));
        assert_eq!(parse_where(".a = true").unwrap().literal, json!(true));
        assert_eq!(parse_where(".a = null").unwrap().literal, Value::Null);
        // Operator-looking content inside quotes stays literal.
        assert_eq!(parse_where(".a = \"a=b\"").unwrap().literal, json!("a=b"));
        assert_eq!(
            parse_where(".a = \"say \\\"hi\\\"\"").unwrap().literal,
            json!("say \"hi\"")
        );
    }

    #[test]
    fn parse_where_rejects() {
        for spec in [
            "",
            ".a",
            ".a =",
            "= 1",
            ".a foo 1",
            ".a = unquoted",
            ".a ~ 1",
            ".a ~ \"(\"",
        ] {
            assert!(parse_where(spec).is_err(), "{spec}");
        }
    }

    #[test]
    fn eval_where_equality() {
        let el = json!({"n": 3, "f": 3.5, "s": "x", "b": true, "z": null});
        let t = |spec: &str| eval_where(&parse_where(spec).unwrap(), &el).unwrap();
        assert!(t(".n = 3"));
        assert!(t(".n = 3.0")); // cross-representation numeric equality
        assert!(!t(".n = 4"));
        assert!(t(".n != 4"));
        assert!(!t(".missing = 1"));
        assert!(t(".missing != 1"));
        assert!(t(".s = \"x\""));
        assert!(!t(".s = \"y\""));
        assert!(!t(".n = \"3\"")); // strict across types
        assert!(t(".f = 3.5"));
        assert!(t(".b = true"));
        assert!(!t(".b = false"));
        assert!(t(".z = null"));
        assert!(!t(".missing = null"));
    }

    #[test]
    fn eval_where_ordering() {
        let el = json!({"n": 3, "s": "b"});
        let t = |spec: &str| eval_where(&parse_where(spec).unwrap(), &el).unwrap();
        assert!(t(".n < 4"));
        assert!(t(".n <= 3"));
        assert!(!t(".n <= 2"));
        assert!(t(".n > 2"));
        assert!(t(".n >= 3"));
        assert!(!t(".n > 3"));
        assert!(t(".s > \"a\""));
        assert!(t(".s <= \"b\""));
        assert!(!t(".s < 1")); // mixed types never order
        assert!(!t(".n < \"x\""));
        assert!(!t(".missing < 1"));
    }

    #[test]
    fn eval_where_regex() {
        let el = json!({"title": "feat(nix): x", "n": 42});
        let t = |spec: &str| eval_where(&parse_where(spec).unwrap(), &el).unwrap();
        assert!(t(".title ~ \"^feat\""));
        assert!(!t(".title ~ \"^fix\""));
        assert!(t(".title !~ \"^fix\""));
        assert!(!t(".title !~ \"^feat\""));
        assert!(t(".n ~ \"^4\"")); // non-strings match compact JSON
        assert!(!t(".missing ~ \"x\""));
        assert!(t(".missing !~ \"x\""));
        assert!(t("/title ~ \"nix\"")); // JSON Pointer path works too
    }

    #[test]
    fn run_where_filters_before_projection() {
        let (_d, p) = write_tmp(
            r#"{"issues":[{"id":1,"priority":1,"status":"open","title":"a"},{"id":2,"priority":5,"status":"open","title":"b"},{"id":3,"priority":1,"status":"closed","title":"c"}]}"#,
        );
        // Two predicates AND together: only id 1 survives.
        let args = where_args(p, &[".priority < 3", ".status = \"open\""]);
        assert_eq!(run(&args).unwrap(), Outcome::Found);
    }

    #[test]
    fn run_where_matching_nothing_is_not_found() {
        let (_d, p) = write_tmp(r#"{"issues":[{"id":1}]}"#);
        let args = where_args(p, &[".id > 9"]);
        assert_eq!(run(&args).unwrap(), Outcome::NotFound);
    }

    #[test]
    fn run_where_bad_expression_is_error() {
        let (_d, p) = write_tmp(r#"{"issues":[{"id":1}]}"#);
        let args = where_args(p, &[".id ~~ 1"]);
        let err = run(&args).unwrap_err().to_string();
        assert!(err.contains("--where"), "{err}");
    }
}
