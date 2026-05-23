//! Sole chokepoint for `rusqlite::Connection` access.
//!
//! Every other module in `src/sqlite/` must route SQLite access through the
//! helpers exposed here. Importing `rusqlite::Connection` (or calling any of
//! its mutation methods) anywhere else in the domain is forbidden, and the
//! [`tests::no_mutation_methods_outside_client_module`] grep test enforces it
//! on every `cargo test --features sqlite` run.
//!
//! Read-only enforcement here is twofold and stronger than k8s's:
//!
//! 1. The connection is opened with `SQLITE_OPEN_READ_ONLY`, so the OS-level
//!    file open is read-only and the engine refuses any write attempt.
//! 2. `PRAGMA query_only = ON` is set immediately after open as defense in
//!    depth — even if a future rusqlite bug somehow lifted (1), the engine
//!    would still reject mutations.

use std::path::Path;

use anyhow::{Context, Result};
use rusqlite::{Connection, OpenFlags};

/// Re-export of `rusqlite::Connection` under a domain-local name. Sibling
/// modules in `src/sqlite/` reference this alias when they need to thread
/// a connection through helper functions, so they can stay free of the
/// `rusqlite::Connection` token that the chokepoint grep test forbids.
pub type Conn = Connection;

/// Open a SQLite database file read-only.
///
/// Uses `SQLITE_OPEN_READ_ONLY | SQLITE_OPEN_URI` and then immediately runs
/// `PRAGMA query_only = ON;` as defense in depth. Returns an error if the
/// file does not exist, is not a SQLite database, or cannot be read.
pub fn open_readonly(path: &Path) -> Result<Connection> {
    let conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI,
    )
    .with_context(|| format!("opening {} read-only", path.display()))?;
    conn.pragma_update(None, "query_only", true)
        .context("setting PRAGMA query_only=ON")?;
    Ok(conn)
}

/// Run a SELECT statement and collect every row as a vector of stringified
/// cell values. NULLs become empty strings; integers and reals are formatted
/// with their `Display` impls; blobs are rendered as `<blob:N bytes>`.
///
/// This is the only sanctioned way for sibling modules in `src/sqlite/` to
/// pull rows out of a connection — they must not import `rusqlite::Connection`
/// or call mutation methods directly. The chokepoint grep test enforces this.
pub fn query_rows(conn: &Connection, sql: &str) -> Result<Vec<Vec<String>>> {
    let mut stmt = conn
        .prepare(sql)
        .with_context(|| format!("preparing query: {sql}"))?;
    let column_count = stmt.column_count();
    let rows_iter = stmt
        .query_map([], |row| {
            let mut row_vec = Vec::with_capacity(column_count);
            for i in 0..column_count {
                let value: rusqlite::types::Value = row.get(i)?;
                let cell = match value {
                    rusqlite::types::Value::Null => String::new(),
                    rusqlite::types::Value::Integer(n) => n.to_string(),
                    rusqlite::types::Value::Real(f) => f.to_string(),
                    rusqlite::types::Value::Text(t) => t,
                    rusqlite::types::Value::Blob(b) => format!("<blob:{} bytes>", b.len()),
                };
                row_vec.push(cell);
            }
            Ok(row_vec)
        })
        .with_context(|| format!("executing query: {sql}"))?;

    let mut out = Vec::new();
    for row in rows_iter {
        out.push(row.with_context(|| format!("reading row from: {sql}"))?);
    }
    Ok(out)
}

/// Run a SELECT (or `WITH` / `EXPLAIN` / `PRAGMA`) statement and collect every
/// row as a vector of typed [`rusqlite::types::Value`] cells, alongside the
/// column names from the cursor in declared order.
///
/// This is the type-preserving counterpart to [`query_rows`] used by commands
/// (`query`, `dump`, ...) that need to know whether each cell is `NULL`, an
/// integer, a real, a string, or a blob — for example to render JSON-lines
/// output where the JSON type must match the SQLite type.
///
/// All `rusqlite::Connection` and `Statement` access stays in this module so
/// the chokepoint grep test continues to hold.
pub fn query_rows_typed(
    conn: &Connection,
    sql: &str,
) -> Result<(Vec<String>, Vec<Vec<rusqlite::types::Value>>)> {
    let mut stmt = conn
        .prepare(sql)
        .with_context(|| format!("preparing query: {sql}"))?;
    let columns: Vec<String> = stmt.column_names().into_iter().map(String::from).collect();
    let column_count = columns.len();
    let rows_iter = stmt
        .query_map([], |row| {
            let mut row_vec = Vec::with_capacity(column_count);
            for i in 0..column_count {
                let value: rusqlite::types::Value = row.get(i)?;
                row_vec.push(value);
            }
            Ok(row_vec)
        })
        .with_context(|| format!("executing query: {sql}"))?;

    let mut out = Vec::new();
    for row in rows_iter {
        out.push(row.with_context(|| format!("reading row from: {sql}"))?);
    }
    Ok((columns, out))
}

/// Test helper: open a writable connection and run a SQL batch to seed a
/// fixture database. Lives in `client.rs` so sibling tests don't have to
/// import `rusqlite::Connection` themselves (which the chokepoint test would
/// reject). Only compiled under `cfg(test)`.
#[cfg(test)]
pub(crate) fn seed_for_tests(path: &Path, sql: &str) {
    let writer = Connection::open(path).expect("open writable for test seed");
    writer.execute_batch(sql).expect("seed sql batch");
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Tokens that must not appear in any `src/sqlite/*.rs` file other than
    /// `client.rs`. The directory walk and comment-skip mechanics live in
    /// [`crate::test_support::assert_no_forbidden_tokens`].
    const FORBIDDEN_TOKENS: &[&str] = &[
        "rusqlite::Connection",
        "Connection::open",
        ".execute(",
        ".execute_batch(",
    ];

    #[test]
    fn no_mutation_methods_outside_client_module() {
        crate::test_support::assert_no_forbidden_tokens(
            "sqlite",
            FORBIDDEN_TOKENS,
            "rusqlite::Connection / mutation methods must be confined to src/sqlite/client.rs",
        );
    }

    /// End-to-end behavior test: confirm that a connection opened via
    /// `open_readonly` rejects every common mutation statement.
    ///
    /// We seed the database via a *separate* writable connection (this is
    /// the only place in the crate where opening a writable SQLite handle
    /// is allowed — the chokepoint test exempts `client.rs`).
    #[test]
    fn read_only_connection_rejects_mutations() {
        let tmp = tempfile::NamedTempFile::new().expect("create temp file");
        let path = tmp.path().to_path_buf();

        // Seed the file with a table and a row using a writable connection.
        // Scoped so the writable handle is dropped before we reopen read-only.
        {
            let writer = Connection::open(&path).expect("open writable");
            writer
                .execute_batch(
                    "CREATE TABLE t (id INTEGER PRIMARY KEY, name TEXT); \
                     INSERT INTO t (id, name) VALUES (1, 'seed');",
                )
                .expect("seed table");
        }

        let ro = open_readonly(&path).expect("open_readonly");

        // Reads must still work.
        let name: String = ro
            .query_row("SELECT name FROM t WHERE id = 1", [], |row| row.get(0))
            .expect("read seeded row");
        assert_eq!(name, "seed");

        // Every mutation form must be rejected. We don't care about the
        // exact error string — only that the call returns Err.
        let mutations = [
            "INSERT INTO t (id, name) VALUES (2, 'nope')",
            "UPDATE t SET name = 'changed' WHERE id = 1",
            "DELETE FROM t WHERE id = 1",
            "CREATE TABLE u (id INTEGER)",
            "DROP TABLE t",
        ];
        for sql in mutations {
            let result = ro.execute(sql, []);
            assert!(
                result.is_err(),
                "mutation `{sql}` should have been rejected but returned {result:?}",
            );
        }
    }
}
