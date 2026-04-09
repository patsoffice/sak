use std::io;
use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{Result, anyhow};
use clap::Args;

use super::client;
use crate::output::BoundedWriter;

#[derive(Args)]
#[command(
    about = "Count rows in a SQLite table, optionally with a WHERE filter",
    long_about = "Count rows in a SQLite table.\n\n\
        Builds and runs `SELECT COUNT(*) FROM \"<table>\"` against a read-only \
        connection, with an optional `--where <expr>` clause appended verbatim. \
        The table name is double-quote escaped so identifiers containing \
        spaces, punctuation, or even embedded double quotes are accepted \
        safely.\n\n\
        Unlike the rest of sak, `count` exits 0 even when the count is zero — \
        the count itself is the result, and an empty table is a valid answer. \
        Errors (missing table, malformed `--where`) still produce exit 2.",
    after_help = "\
Examples:
  sak sqlite count app.db users
  sak sqlite count app.db users --where 'active = 1'
  sak sqlite count app.db 'order items'"
)]
pub struct CountArgs {
    /// Path to the SQLite database file
    pub db: PathBuf,

    /// Table (or view) name
    pub table: String,

    /// Optional SQL boolean expression appended as `WHERE <expr>` (no leading `WHERE`)
    #[arg(long = "where", value_name = "EXPR")]
    pub where_clause: Option<String>,
}

pub fn run(args: &CountArgs) -> Result<ExitCode> {
    if args.table.contains('\0') {
        return Err(anyhow!("table name must not contain NUL bytes"));
    }
    if let Some(w) = &args.where_clause {
        if w.contains(';') {
            return Err(anyhow!(
                "--where must not contain `;` (statement chaining is not allowed)"
            ));
        }
        if w.contains('\0') {
            return Err(anyhow!("--where must not contain NUL bytes"));
        }
    }

    let quoted = quote_ident(&args.table);
    let sql = match &args.where_clause {
        Some(w) => format!("SELECT COUNT(*) FROM {quoted} WHERE {w}"),
        None => format!("SELECT COUNT(*) FROM {quoted}"),
    };

    let conn = client::open_readonly(&args.db)?;
    let rows = client::query_rows(&conn, &sql)?;
    let count = rows
        .first()
        .and_then(|r| r.first())
        .ok_or_else(|| anyhow!("COUNT(*) returned no rows"))?;

    let stdout = io::stdout();
    let handle = stdout.lock();
    let mut writer = BoundedWriter::new(handle, None);
    writer.write_line(count)?;
    writer.flush()?;

    Ok(ExitCode::SUCCESS)
}

/// Wrap an identifier in double quotes, doubling any internal double quotes —
/// the SQL standard form. Public-in-crate so the `dump` command can reuse it.
pub(crate) fn quote_ident(ident: &str) -> String {
    let escaped = ident.replace('"', "\"\"");
    format!("\"{escaped}\"")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seeded_db() -> tempfile::NamedTempFile {
        let tmp = tempfile::NamedTempFile::new().expect("temp file");
        client::seed_for_tests(
            tmp.path(),
            "CREATE TABLE users (id INTEGER PRIMARY KEY, active INTEGER); \
             INSERT INTO users VALUES (1, 1), (2, 0), (3, 1); \
             CREATE TABLE empty_t (id INTEGER); \
             CREATE TABLE \"weird\"\"name\" (id INTEGER); \
             INSERT INTO \"weird\"\"name\" VALUES (1);",
        );
        tmp
    }

    fn args(db: &std::path::Path, table: &str, where_clause: Option<&str>) -> CountArgs {
        CountArgs {
            db: db.to_path_buf(),
            table: table.to_string(),
            where_clause: where_clause.map(String::from),
        }
    }

    #[test]
    fn quote_ident_doubles_internal_quotes() {
        assert_eq!(quote_ident("users"), "\"users\"");
        assert_eq!(quote_ident("weird\"name"), "\"weird\"\"name\"");
    }

    #[test]
    fn counts_non_empty_table() {
        let tmp = seeded_db();
        let conn = client::open_readonly(tmp.path()).unwrap();
        let rows = client::query_rows(&conn, "SELECT COUNT(*) FROM \"users\"").unwrap();
        assert_eq!(rows[0][0], "3");
    }

    #[test]
    fn run_succeeds_on_empty_table() {
        let tmp = seeded_db();
        let code = run(&args(tmp.path(), "empty_t", None)).unwrap();
        assert_eq!(code, ExitCode::SUCCESS);
    }

    #[test]
    fn run_succeeds_on_populated_table() {
        let tmp = seeded_db();
        let code = run(&args(tmp.path(), "users", None)).unwrap();
        assert_eq!(code, ExitCode::SUCCESS);
    }

    #[test]
    fn where_filter_applies() {
        let tmp = seeded_db();
        let conn = client::open_readonly(tmp.path()).unwrap();
        let rows =
            client::query_rows(&conn, "SELECT COUNT(*) FROM \"users\" WHERE active = 1").unwrap();
        assert_eq!(rows[0][0], "2");
    }

    #[test]
    fn run_with_where_succeeds() {
        let tmp = seeded_db();
        let code = run(&args(tmp.path(), "users", Some("active = 1"))).unwrap();
        assert_eq!(code, ExitCode::SUCCESS);
    }

    #[test]
    fn run_errors_on_missing_table() {
        let tmp = seeded_db();
        let result = run(&args(tmp.path(), "no_such_table", None));
        assert!(result.is_err());
    }

    #[test]
    fn rejects_semicolon_in_where() {
        let tmp = seeded_db();
        let result = run(&args(tmp.path(), "users", Some("1=1; DROP TABLE users")));
        let err = result.unwrap_err().to_string();
        assert!(err.contains(';'), "expected semicolon error, got: {err}");
    }

    #[test]
    fn rejects_nul_in_table() {
        let tmp = seeded_db();
        let result = run(&args(tmp.path(), "users\0", None));
        assert!(result.is_err());
    }

    #[test]
    fn handles_table_with_quoted_double_quote() {
        let tmp = seeded_db();
        // The table is literally named  weird"name  — quote_ident must
        // double the embedded `"` so SQLite sees the right identifier.
        let code = run(&args(tmp.path(), "weird\"name", None)).unwrap();
        assert_eq!(code, ExitCode::SUCCESS);
    }

    #[test]
    fn run_errors_on_missing_db() {
        let result = run(&args(
            std::path::Path::new("/nonexistent/path.db"),
            "users",
            None,
        ));
        assert!(result.is_err());
    }
}
