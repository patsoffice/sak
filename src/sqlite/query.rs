use std::io;
use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{Result, anyhow};
use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use clap::{Args, ValueEnum};
use rusqlite::types::Value;

use super::client;
use crate::output::BoundedWriter;

#[derive(Args)]
#[command(
    about = "Run a read-only SQL query against a SQLite database",
    long_about = "Run a read-only SQL statement against a SQLite database file.\n\n\
        Only `SELECT`, `WITH`, `EXPLAIN`, and `PRAGMA` statements are accepted. \
        Other statement forms are rejected before the query reaches the engine — \
        belt-and-suspenders defense in depth on top of the read-only connection \
        opened by the sqlite domain.\n\n\
        Output is JSON-lines by default (one object per row, keys in column order). \
        Pass `--format tsv` for a header row plus tab-separated values. \
        SQLite type → output mapping: NULL → null / empty, INTEGER and REAL → \
        JSON numbers, TEXT → JSON strings, BLOB → base64-encoded strings.",
    after_help = "\
Examples:
  sak sqlite query app.db 'SELECT id, name FROM users LIMIT 5'
  sak sqlite query app.db 'PRAGMA user_version'
  sak sqlite query app.db 'SELECT * FROM users' --format tsv
  sak sqlite query app.db 'WITH t AS (SELECT 1 AS n) SELECT * FROM t'"
)]
pub struct QueryArgs {
    /// Path to the SQLite database file
    pub db: PathBuf,

    /// SQL statement (must begin with SELECT, WITH, EXPLAIN, or PRAGMA)
    pub sql: String,

    /// Output format
    #[arg(long, value_enum, default_value_t = OutputFormat::Json)]
    pub format: OutputFormat,

    /// Maximum number of rows to emit
    #[arg(long)]
    pub limit: Option<usize>,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, ValueEnum)]
pub enum OutputFormat {
    /// JSON-lines: one object per row, keys in column order
    Json,
    /// Tab-separated values with a header row
    Tsv,
}

pub fn run(args: &QueryArgs) -> Result<ExitCode> {
    if !is_read_statement(&args.sql) {
        return Err(anyhow!(
            "only SELECT, WITH, EXPLAIN, and PRAGMA statements are allowed (after \
             stripping leading whitespace and SQL comments)"
        ));
    }

    let conn = client::open_readonly(&args.db)?;
    let (columns, rows) = client::query_rows_typed(&conn, &args.sql)?;

    let stdout = io::stdout();
    let handle = stdout.lock();
    let mut writer = BoundedWriter::new(handle, args.limit);

    if args.format == OutputFormat::Tsv {
        // The header row is decoration so `--limit` caps data rows, matching
        // user expectations ("--limit 5 gives me 5 rows of data").
        writer.write_decoration(&columns.join("\t"))?;
    }

    let mut emitted = 0usize;
    for row in &rows {
        let line = match args.format {
            OutputFormat::Json => row_to_json_line(&columns, row),
            OutputFormat::Tsv => row_to_tsv_line(row),
        };
        if !writer.write_line(&line)? {
            break;
        }
        emitted += 1;
    }
    writer.flush()?;

    if emitted == 0 {
        Ok(ExitCode::from(1))
    } else {
        Ok(ExitCode::SUCCESS)
    }
}

/// Reject anything that isn't a read-only statement form. The connection is
/// already opened read-only and `PRAGMA query_only=ON`, but rejecting writes
/// here lets the caller produce a clean sak-style error instead of an opaque
/// "attempt to write a readonly database" message from sqlite.
pub(crate) fn is_read_statement(sql: &str) -> bool {
    let stripped = strip_leading_comments_and_ws(sql);
    if stripped.is_empty() {
        return false;
    }
    let lower = stripped.to_ascii_lowercase();
    for kw in ["select", "with", "explain", "pragma"] {
        if let Some(rest) = lower.strip_prefix(kw) {
            // Next char must be whitespace or `(` (or end of string) so that
            // identifiers like `selected_at` aren't accepted.
            match rest.as_bytes().first() {
                None => return true,
                Some(b' ' | b'\t' | b'\n' | b'\r' | b'(') => return true,
                _ => {}
            }
        }
    }
    false
}

/// Trim leading whitespace and SQL line/block comments. Stops at the first
/// character that is neither whitespace nor the start of a comment. An
/// unterminated block comment collapses the rest of the input to "" so the
/// statement gate rejects it as empty.
fn strip_leading_comments_and_ws(sql: &str) -> &str {
    let mut s = sql.trim_start();
    loop {
        if let Some(rest) = s.strip_prefix("--") {
            s = match rest.find('\n') {
                Some(i) => rest[i + 1..].trim_start(),
                None => "",
            };
        } else if let Some(rest) = s.strip_prefix("/*") {
            s = match rest.find("*/") {
                Some(i) => rest[i + 2..].trim_start(),
                None => "",
            };
        } else {
            return s;
        }
    }
}

/// Render one row as a JSON object literal with keys in column order. We
/// assemble the line manually rather than going through `serde_json::Map`
/// because the default `serde_json` features sort object keys
/// alphabetically — and the column order from the cursor is the contract.
pub(crate) fn row_to_json_line(columns: &[String], row: &[Value]) -> String {
    let mut s = String::from("{");
    for (i, (col, val)) in columns.iter().zip(row.iter()).enumerate() {
        if i > 0 {
            s.push(',');
        }
        // serde_json::to_string on a &String emits a properly escaped JSON
        // string literal — handles quotes, control chars, unicode, etc.
        s.push_str(&serde_json::to_string(col).expect("column name JSON-safe"));
        s.push(':');
        s.push_str(&value_to_json(val));
    }
    s.push('}');
    s
}

/// Map a single SQLite cell to its JSON representation.
fn value_to_json(v: &Value) -> String {
    match v {
        Value::Null => "null".into(),
        Value::Integer(n) => n.to_string(),
        Value::Real(f) => {
            // JSON numbers cannot represent NaN/Infinity. `Number::from_f64`
            // returns None in that case — fall back to null so the line stays
            // valid JSON.
            serde_json::Number::from_f64(*f)
                .map(|n| n.to_string())
                .unwrap_or_else(|| "null".into())
        }
        Value::Text(t) => serde_json::to_string(t).expect("text JSON-safe"),
        Value::Blob(b) => {
            let encoded = B64.encode(b);
            serde_json::to_string(&encoded).expect("base64 JSON-safe")
        }
    }
}

/// Render one row as a single tab-separated line. NULL → empty string;
/// numbers via Display; text with `\`, `\n`, `\t`, `\r` escaped so a single
/// row never spans multiple output lines; blobs as base64.
pub(crate) fn row_to_tsv_line(row: &[Value]) -> String {
    row.iter()
        .map(value_to_tsv_cell)
        .collect::<Vec<_>>()
        .join("\t")
}

fn value_to_tsv_cell(v: &Value) -> String {
    match v {
        Value::Null => String::new(),
        Value::Integer(n) => n.to_string(),
        Value::Real(f) => f.to_string(),
        Value::Text(t) => {
            // Escape backslash first so we don't double-escape the
            // backslashes we add for \n / \t / \r.
            t.replace('\\', "\\\\")
                .replace('\n', "\\n")
                .replace('\t', "\\t")
                .replace('\r', "\\r")
        }
        Value::Blob(b) => B64.encode(b),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- is_read_statement ----

    #[test]
    fn accepts_plain_select() {
        assert!(is_read_statement("SELECT * FROM t"));
        assert!(is_read_statement("select 1"));
    }

    #[test]
    fn accepts_with_cte() {
        assert!(is_read_statement(
            "  WITH cte AS (SELECT 1) SELECT * FROM cte"
        ));
    }

    #[test]
    fn accepts_explain_and_pragma() {
        assert!(is_read_statement("EXPLAIN SELECT 1"));
        assert!(is_read_statement("PRAGMA user_version"));
    }

    #[test]
    fn accepts_after_line_comment() {
        assert!(is_read_statement("-- comment\nSELECT 1"));
    }

    #[test]
    fn accepts_after_block_comment() {
        assert!(is_read_statement("/* block */ SELECT 1"));
        assert!(is_read_statement("/* multi\nline */ SELECT 1"));
    }

    #[test]
    fn rejects_mutations() {
        assert!(!is_read_statement("INSERT INTO t VALUES (1)"));
        assert!(!is_read_statement("UPDATE t SET x = 1"));
        assert!(!is_read_statement("DELETE FROM t"));
        assert!(!is_read_statement("DROP TABLE t"));
        assert!(!is_read_statement("CREATE TABLE u (id INTEGER)"));
        assert!(!is_read_statement("ATTACH DATABASE 'x' AS x"));
    }

    #[test]
    fn rejects_empty_or_whitespace() {
        assert!(!is_read_statement(""));
        assert!(!is_read_statement("   "));
        assert!(!is_read_statement("-- only a comment"));
        assert!(!is_read_statement("/* unterminated"));
    }

    #[test]
    fn rejects_identifier_prefixed_with_keyword() {
        // `selected_at` must not be parsed as `select`.
        assert!(!is_read_statement("selected_at"));
        assert!(!is_read_statement("withdraw"));
    }

    // ---- value_to_json ----

    #[test]
    fn json_null() {
        assert_eq!(value_to_json(&Value::Null), "null");
    }

    #[test]
    fn json_integer() {
        assert_eq!(value_to_json(&Value::Integer(42)), "42");
        assert_eq!(value_to_json(&Value::Integer(-7)), "-7");
    }

    #[test]
    fn json_real() {
        assert_eq!(value_to_json(&Value::Real(1.5)), "1.5");
        // NaN/Inf must collapse to null so the line stays valid JSON.
        assert_eq!(value_to_json(&Value::Real(f64::NAN)), "null");
        assert_eq!(value_to_json(&Value::Real(f64::INFINITY)), "null");
    }

    #[test]
    fn json_text_escapes_quotes_and_newlines() {
        let v = Value::Text("he said \"hi\"\nthere".into());
        assert_eq!(value_to_json(&v), "\"he said \\\"hi\\\"\\nthere\"");
    }

    #[test]
    fn json_blob_base64() {
        // bytes 0..3 → base64 "AAEC"
        let v = Value::Blob(vec![0u8, 1, 2]);
        assert_eq!(value_to_json(&v), "\"AAEC\"");
    }

    // ---- row_to_json_line ----

    #[test]
    fn json_line_preserves_column_order() {
        let cols = vec!["z".to_string(), "a".to_string(), "m".to_string()];
        let row = vec![Value::Integer(1), Value::Text("alpha".into()), Value::Null];
        assert_eq!(
            row_to_json_line(&cols, &row),
            r#"{"z":1,"a":"alpha","m":null}"#
        );
    }

    #[test]
    fn json_line_quotes_column_names_with_special_chars() {
        let cols = vec!["he\"y".to_string()];
        let row = vec![Value::Integer(1)];
        assert_eq!(row_to_json_line(&cols, &row), r#"{"he\"y":1}"#);
    }

    // ---- value_to_tsv_cell ----

    #[test]
    fn tsv_null_is_empty() {
        assert_eq!(value_to_tsv_cell(&Value::Null), "");
    }

    #[test]
    fn tsv_numbers() {
        assert_eq!(value_to_tsv_cell(&Value::Integer(42)), "42");
        assert_eq!(value_to_tsv_cell(&Value::Real(1.5)), "1.5");
    }

    #[test]
    fn tsv_text_escapes_control_chars() {
        let v = Value::Text("line1\nline2\twith\\slash".into());
        assert_eq!(value_to_tsv_cell(&v), "line1\\nline2\\twith\\\\slash");
    }

    #[test]
    fn tsv_blob_base64() {
        let v = Value::Blob(vec![0u8, 1, 2]);
        assert_eq!(value_to_tsv_cell(&v), "AAEC");
    }

    #[test]
    fn tsv_line_joins_with_tabs() {
        let row = vec![
            Value::Integer(1),
            Value::Text("a".into()),
            Value::Null,
            Value::Real(2.5),
        ];
        assert_eq!(row_to_tsv_line(&row), "1\ta\t\t2.5");
    }

    // ---- end-to-end against a temp database ----

    fn seeded_db() -> tempfile::NamedTempFile {
        let tmp = tempfile::NamedTempFile::new().expect("temp file");
        client::seed_for_tests(
            tmp.path(),
            "CREATE TABLE all_types (\
                i INTEGER, \
                r REAL, \
                t TEXT, \
                b BLOB, \
                n INTEGER\
             ); \
             INSERT INTO all_types VALUES (7, 1.5, 'hi', x'000102', NULL); \
             INSERT INTO all_types VALUES (8, 2.25, 'bye', x'ff', NULL);",
        );
        tmp
    }

    fn args(db: &std::path::Path, sql: &str, format: OutputFormat) -> QueryArgs {
        QueryArgs {
            db: db.to_path_buf(),
            sql: sql.to_string(),
            format,
            limit: None,
        }
    }

    #[test]
    fn run_rejects_mutation_statement() {
        let tmp = seeded_db();
        let result = run(&args(
            tmp.path(),
            "INSERT INTO all_types VALUES (9, 0, 'x', NULL, NULL)",
            OutputFormat::Json,
        ));
        assert!(result.is_err());
    }

    #[test]
    fn run_emits_success_for_select() {
        let tmp = seeded_db();
        let code = run(&args(
            tmp.path(),
            "SELECT i, r, t, b, n FROM all_types ORDER BY i",
            OutputFormat::Json,
        ))
        .unwrap();
        assert_eq!(code, ExitCode::SUCCESS);
    }

    #[test]
    fn run_emits_exit_one_for_zero_rows() {
        let tmp = seeded_db();
        let code = run(&args(
            tmp.path(),
            "SELECT * FROM all_types WHERE i = 9999",
            OutputFormat::Json,
        ))
        .unwrap();
        assert_eq!(code, ExitCode::from(1));
    }

    #[test]
    fn run_emits_success_for_tsv() {
        let tmp = seeded_db();
        let code = run(&args(
            tmp.path(),
            "SELECT i, r, t, b, n FROM all_types ORDER BY i",
            OutputFormat::Tsv,
        ))
        .unwrap();
        assert_eq!(code, ExitCode::SUCCESS);
    }

    #[test]
    fn typed_rows_round_trip_all_sqlite_types() {
        // Verify the client helper itself returns each SQLite type as the
        // expected `rusqlite::types::Value` variant — the type-mapping unit
        // tests above then cover the rendering side.
        let tmp = seeded_db();
        let conn = client::open_readonly(tmp.path()).unwrap();
        let (cols, rows) =
            client::query_rows_typed(&conn, "SELECT i, r, t, b, n FROM all_types WHERE i = 7")
                .unwrap();
        assert_eq!(cols, vec!["i", "r", "t", "b", "n"]);
        assert_eq!(rows.len(), 1);
        let row = &rows[0];
        assert!(matches!(row[0], Value::Integer(7)));
        assert!(matches!(row[1], Value::Real(x) if (x - 1.5).abs() < 1e-12));
        assert!(matches!(row[2], Value::Text(ref s) if s == "hi"));
        assert!(matches!(row[3], Value::Blob(ref b) if b == &vec![0u8, 1, 2]));
        assert!(matches!(row[4], Value::Null));
    }

    #[test]
    fn run_errors_on_missing_db() {
        let result = run(&args(
            std::path::Path::new("/nonexistent/path.db"),
            "SELECT 1",
            OutputFormat::Json,
        ));
        assert!(result.is_err());
    }
}
