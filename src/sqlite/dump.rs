use std::io;
use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{Result, anyhow};
use clap::Args;

use super::client;
use super::count::quote_ident;
use super::query::{OutputFormat, row_to_json_line, row_to_tsv_line};
use crate::output::BoundedWriter;

#[derive(Args)]
#[command(
    about = "Dump rows from a SQLite table",
    long_about = "Dump up to N rows from a SQLite table or view.\n\n\
        Friendlier-flag wrapper around `SELECT * FROM <table> [ORDER BY ...] LIMIT N` \
        that handles identifier quoting so an LLM never has to construct SQL by \
        hand. The table and `--order-by` column are double-quote escaped — \
        identifiers with spaces, punctuation, or embedded double quotes are \
        accepted safely.\n\n\
        Output format and SQLite-type → JSON/TSV mapping match `sak sqlite query`: \
        JSON-lines by default, `--format tsv` for a header row plus tab-separated \
        values, NULL → null/empty, INTEGER and REAL → JSON numbers, TEXT → JSON \
        strings, BLOB → base64.",
    after_help = "\
Examples:
  sak sqlite dump app.db users
  sak sqlite dump app.db users --limit 5
  sak sqlite dump app.db users --order-by id --desc
  sak sqlite dump app.db users --format tsv --limit 100"
)]
pub struct DumpArgs {
    /// Path to the SQLite database file
    pub db: PathBuf,

    /// Table or view name
    pub table: String,

    /// Maximum number of rows to emit (default 10)
    #[arg(long, default_value_t = 10)]
    pub limit: usize,

    /// Column to ORDER BY (quoted automatically)
    #[arg(long)]
    pub order_by: Option<String>,

    /// Order descending instead of ascending (no-op without --order-by)
    #[arg(long)]
    pub desc: bool,

    /// Output format
    #[arg(long, value_enum, default_value_t = OutputFormat::Json)]
    pub format: OutputFormat,
}

pub fn run(args: &DumpArgs) -> Result<ExitCode> {
    if args.table.contains('\0') {
        return Err(anyhow!("table name must not contain NUL bytes"));
    }
    if let Some(col) = &args.order_by
        && col.contains('\0')
    {
        return Err(anyhow!("--order-by must not contain NUL bytes"));
    }
    if args.limit == 0 {
        return Err(anyhow!("--limit must be a positive integer"));
    }

    let conn = client::open_readonly(&args.db)?;

    let table_quoted = quote_ident(&args.table);

    // SQLite has a legacy quirk where an unrecognized double-quoted identifier
    // is silently re-interpreted as a string literal — meaning a typo'd
    // `ORDER BY "no_such_col"` would NOT raise an error, it would order by
    // a constant. Validate the column up-front against `PRAGMA table_info` so
    // sak surfaces a clean error (matching the issue's exit-code-2 contract).
    if let Some(col) = &args.order_by {
        let info_sql = format!("PRAGMA table_info({table_quoted})");
        let info_rows = client::query_rows(&conn, &info_sql)?;
        if info_rows.is_empty() {
            return Err(anyhow!("table {} not found", args.table));
        }
        let known: Vec<&str> = info_rows.iter().map(|r| r[1].as_str()).collect();
        if !known.iter().any(|n| n.eq_ignore_ascii_case(col)) {
            return Err(anyhow!(
                "column {col:?} not found in table {} (have: {})",
                args.table,
                known.join(", ")
            ));
        }
    }

    let order_clause = match &args.order_by {
        Some(col) => {
            let dir = if args.desc { " DESC" } else { "" };
            format!(" ORDER BY {}{}", quote_ident(col), dir)
        }
        None => String::new(),
    };
    let sql = format!(
        "SELECT * FROM {table_quoted}{order_clause} LIMIT {}",
        args.limit
    );

    let (columns, rows) = client::query_rows_typed(&conn, &sql)?;

    let stdout = io::stdout();
    let handle = stdout.lock();
    let mut writer = BoundedWriter::new(handle, None);

    if args.format == OutputFormat::Tsv {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn seeded_db() -> tempfile::NamedTempFile {
        let tmp = tempfile::NamedTempFile::new().expect("temp file");
        client::seed_for_tests(
            tmp.path(),
            "CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT); \
             INSERT INTO users VALUES (1, 'alice'), (2, 'bob'), (3, 'carol'); \
             CREATE TABLE empty_t (id INTEGER);",
        );
        tmp
    }

    fn args(db: &std::path::Path, table: &str) -> DumpArgs {
        DumpArgs {
            db: db.to_path_buf(),
            table: table.to_string(),
            limit: 10,
            order_by: None,
            desc: false,
            format: OutputFormat::Json,
        }
    }

    #[test]
    fn ascending_order_by_id() {
        let tmp = seeded_db();
        let conn = client::open_readonly(tmp.path()).unwrap();
        let (_, rows) =
            client::query_rows_typed(&conn, "SELECT * FROM \"users\" ORDER BY \"id\" LIMIT 10")
                .unwrap();
        assert_eq!(rows.len(), 3);
        assert!(matches!(rows[0][0], rusqlite::types::Value::Integer(1)));
        assert!(matches!(rows[2][0], rusqlite::types::Value::Integer(3)));
    }

    #[test]
    fn descending_order_by_id() {
        let tmp = seeded_db();
        let conn = client::open_readonly(tmp.path()).unwrap();
        let (_, rows) = client::query_rows_typed(
            &conn,
            "SELECT * FROM \"users\" ORDER BY \"id\" DESC LIMIT 10",
        )
        .unwrap();
        assert!(matches!(rows[0][0], rusqlite::types::Value::Integer(3)));
        assert!(matches!(rows[2][0], rusqlite::types::Value::Integer(1)));
    }

    #[test]
    fn run_default_limit_returns_success() {
        let tmp = seeded_db();
        let code = run(&args(tmp.path(), "users")).unwrap();
        assert_eq!(code, ExitCode::SUCCESS);
    }

    #[test]
    fn run_explicit_limit_returns_success() {
        let tmp = seeded_db();
        let mut a = args(tmp.path(), "users");
        a.limit = 2;
        let code = run(&a).unwrap();
        assert_eq!(code, ExitCode::SUCCESS);
    }

    #[test]
    fn run_with_order_by_and_desc() {
        let tmp = seeded_db();
        let mut a = args(tmp.path(), "users");
        a.order_by = Some("id".into());
        a.desc = true;
        let code = run(&a).unwrap();
        assert_eq!(code, ExitCode::SUCCESS);
    }

    #[test]
    fn missing_column_yields_error() {
        let tmp = seeded_db();
        let mut a = args(tmp.path(), "users");
        a.order_by = Some("no_such_column".into());
        let result = run(&a);
        assert!(result.is_err());
    }

    #[test]
    fn empty_table_yields_exit_one() {
        let tmp = seeded_db();
        let code = run(&args(tmp.path(), "empty_t")).unwrap();
        assert_eq!(code, ExitCode::from(1));
    }

    #[test]
    fn missing_table_yields_error() {
        let tmp = seeded_db();
        let result = run(&args(tmp.path(), "no_such_table"));
        assert!(result.is_err());
    }

    #[test]
    fn rejects_nul_in_table() {
        let tmp = seeded_db();
        let result = run(&args(tmp.path(), "users\0"));
        assert!(result.is_err());
    }

    #[test]
    fn rejects_nul_in_order_by() {
        let tmp = seeded_db();
        let mut a = args(tmp.path(), "users");
        a.order_by = Some("id\0".into());
        let result = run(&a);
        assert!(result.is_err());
    }

    #[test]
    fn rejects_zero_limit() {
        let tmp = seeded_db();
        let mut a = args(tmp.path(), "users");
        a.limit = 0;
        let result = run(&a);
        assert!(result.is_err());
    }
}
