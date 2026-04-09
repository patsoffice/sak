use std::io;
use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{Result, anyhow};
use clap::Args;

use super::client;
use crate::output::BoundedWriter;

#[derive(Args)]
#[command(
    about = "Show the schema of a SQLite database",
    long_about = "Show the schema of a SQLite database file.\n\n\
        Without a `[table]` argument, dumps every user-defined object's \
        `CREATE` statement from `sqlite_master` (tables, views, indexes, \
        triggers), grouped by section header. With a `[table]` argument, \
        shows the column list (from `PRAGMA table_info`) and the index \
        list (from `PRAGMA index_list`) for that single table or view.\n\n\
        Section headers (`# table users`, `# Columns`, `# Indexes`) are \
        emitted as decorations and do not count toward `--limit`.",
    after_help = "\
Examples:
  sak sqlite schema app.db                Dump CREATE statements for every object
  sak sqlite schema app.db users          Show columns and indexes for `users`"
)]
pub struct SchemaArgs {
    /// Path to the SQLite database file
    pub db: PathBuf,

    /// Optional table or view name. If omitted, dumps all schema.
    pub table: Option<String>,

    /// Maximum output lines (decoration headers are not counted)
    #[arg(long)]
    pub limit: Option<usize>,
}

pub fn run(args: &SchemaArgs) -> Result<ExitCode> {
    let conn = client::open_readonly(&args.db)?;
    match &args.table {
        None => run_dump(&conn, args.limit),
        Some(table) => run_table(&conn, table, args.limit),
    }
}

fn run_dump(conn: &client::Conn, limit: Option<usize>) -> Result<ExitCode> {
    let rows = client::query_rows(
        conn,
        "SELECT type, name, sql FROM sqlite_master \
         WHERE sql IS NOT NULL AND name NOT LIKE 'sqlite_%' \
         ORDER BY name",
    )?;
    if rows.is_empty() {
        return Ok(ExitCode::from(1));
    }

    let stdout = io::stdout();
    let handle = stdout.lock();
    let mut writer = BoundedWriter::new(handle, limit);

    'outer: for row in &rows {
        writer.write_decoration(&format!("# {} {}", row[0], row[1]))?;
        for line in row[2].lines() {
            if !writer.write_line(line)? {
                break 'outer;
            }
        }
    }
    writer.flush()?;
    Ok(ExitCode::SUCCESS)
}

fn run_table(conn: &client::Conn, table: &str, limit: Option<usize>) -> Result<ExitCode> {
    // Pre-flight: confirm the table or view actually exists. PRAGMA
    // table_info on a missing table returns an empty result with no error,
    // and we want a clear "not found" exit instead.
    let known = client::query_rows(
        conn,
        "SELECT name FROM sqlite_master WHERE type IN ('table', 'view') ORDER BY name",
    )?;
    if !known.iter().any(|r| r[0] == table) {
        return Err(anyhow!("table not found: {table}"));
    }

    let quoted = quote_identifier(table)?;
    let cols = client::query_rows(conn, &format!("PRAGMA table_info({quoted})"))?;
    let idxs = client::query_rows(conn, &format!("PRAGMA index_list({quoted})"))?;

    if cols.is_empty() && idxs.is_empty() {
        return Ok(ExitCode::from(1));
    }

    let stdout = io::stdout();
    let handle = stdout.lock();
    let mut writer = BoundedWriter::new(handle, limit);

    writer.write_decoration("# Columns")?;
    writer.write_line("cid\tname\ttype\tnotnull\tdflt\tpk")?;
    'cols: for row in &cols {
        // PRAGMA table_info columns: cid, name, type, notnull, dflt_value, pk
        let line = format!(
            "{}\t{}\t{}\t{}\t{}\t{}",
            row[0], row[1], row[2], row[3], row[4], row[5]
        );
        if !writer.write_line(&line)? {
            break 'cols;
        }
    }

    writer.write_decoration("# Indexes")?;
    writer.write_line("seq\tname\tunique\torigin\tpartial")?;
    'idxs: for row in &idxs {
        // PRAGMA index_list columns: seq, name, unique, origin, partial
        let line = format!("{}\t{}\t{}\t{}\t{}", row[0], row[1], row[2], row[3], row[4]);
        if !writer.write_line(&line)? {
            break 'idxs;
        }
    }

    writer.flush()?;
    Ok(ExitCode::SUCCESS)
}

/// Quote a SQLite identifier for safe interpolation into a PRAGMA call.
/// PRAGMA arguments do not accept bound parameters, so identifier-style
/// quoting (double quotes with embedded `"` doubled) is the only safe path.
fn quote_identifier(name: &str) -> Result<String> {
    if name.contains('\0') {
        return Err(anyhow!("table name contains NUL byte"));
    }
    Ok(format!("\"{}\"", name.replace('"', "\"\"")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seeded_db() -> tempfile::NamedTempFile {
        let tmp = tempfile::NamedTempFile::new().expect("temp file");
        client::seed_for_tests(
            tmp.path(),
            "CREATE TABLE users (\
                id INTEGER NOT NULL, \
                tenant INTEGER NOT NULL, \
                email TEXT, \
                PRIMARY KEY (id, tenant)\
             ); \
             CREATE INDEX idx_users_email ON users(email); \
             CREATE TABLE orders (id INTEGER PRIMARY KEY, user_id INTEGER); \
             CREATE VIEW recent_users AS SELECT * FROM users;",
        );
        tmp
    }

    fn args(db: &std::path::Path, table: Option<&str>) -> SchemaArgs {
        SchemaArgs {
            db: db.to_path_buf(),
            table: table.map(String::from),
            limit: None,
        }
    }

    #[test]
    fn quote_identifier_wraps_in_double_quotes() {
        assert_eq!(quote_identifier("users").unwrap(), "\"users\"");
    }

    #[test]
    fn quote_identifier_doubles_internal_quotes() {
        assert_eq!(quote_identifier("we\"ird").unwrap(), "\"we\"\"ird\"");
    }

    #[test]
    fn quote_identifier_rejects_nul() {
        assert!(quote_identifier("bad\0name").is_err());
    }

    #[test]
    fn dump_lists_every_user_object() {
        let tmp = seeded_db();
        let conn = client::open_readonly(tmp.path()).unwrap();
        let rows = client::query_rows(
            &conn,
            "SELECT type, name FROM sqlite_master \
             WHERE sql IS NOT NULL AND name NOT LIKE 'sqlite_%' \
             ORDER BY name",
        )
        .unwrap();
        let entries: Vec<(&str, &str)> = rows
            .iter()
            .map(|r| (r[0].as_str(), r[1].as_str()))
            .collect();
        assert!(entries.contains(&("index", "idx_users_email")));
        assert!(entries.contains(&("table", "orders")));
        assert!(entries.contains(&("table", "users")));
        assert!(entries.contains(&("view", "recent_users")));
    }

    #[test]
    fn run_dump_returns_success_when_schema_present() {
        let tmp = seeded_db();
        let result = run(&args(tmp.path(), None)).unwrap();
        assert_eq!(result, ExitCode::SUCCESS);
    }

    #[test]
    fn run_dump_returns_exit_one_when_empty() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        client::seed_for_tests(tmp.path(), "PRAGMA user_version = 1;");
        let result = run(&args(tmp.path(), None)).unwrap();
        assert_eq!(result, ExitCode::from(1));
    }

    #[test]
    fn table_info_reports_columns_and_pk_membership() {
        let tmp = seeded_db();
        let conn = client::open_readonly(tmp.path()).unwrap();
        let rows = client::query_rows(&conn, "PRAGMA table_info(\"users\")").unwrap();
        // Each row: cid, name, type, notnull, dflt_value, pk
        let by_name: std::collections::HashMap<&str, &Vec<String>> =
            rows.iter().map(|r| (r[1].as_str(), r)).collect();

        let id = by_name.get("id").expect("id column");
        assert_eq!(id[2], "INTEGER");
        assert_eq!(id[3], "1"); // notnull
        assert_ne!(id[5], "0"); // pk position > 0

        let tenant = by_name.get("tenant").expect("tenant column");
        assert_ne!(tenant[5], "0"); // multi-column PK

        let email = by_name.get("email").expect("email column");
        assert_eq!(email[3], "0"); // nullable
        assert_eq!(email[5], "0"); // not part of pk
    }

    #[test]
    fn index_list_reports_user_indexes() {
        let tmp = seeded_db();
        let conn = client::open_readonly(tmp.path()).unwrap();
        let rows = client::query_rows(&conn, "PRAGMA index_list(\"users\")").unwrap();
        let names: Vec<&str> = rows.iter().map(|r| r[1].as_str()).collect();
        assert!(
            names.contains(&"idx_users_email"),
            "expected idx_users_email in {names:?}"
        );
    }

    #[test]
    fn run_table_returns_success_for_known_table() {
        let tmp = seeded_db();
        let result = run(&args(tmp.path(), Some("users"))).unwrap();
        assert_eq!(result, ExitCode::SUCCESS);
    }

    #[test]
    fn run_table_errors_for_missing_table() {
        let tmp = seeded_db();
        let result = run(&args(tmp.path(), Some("nope")));
        assert!(result.is_err());
    }

    #[test]
    fn run_errors_on_missing_db() {
        let result = run(&args(std::path::Path::new("/nonexistent/path.db"), None));
        assert!(result.is_err());
    }
}
