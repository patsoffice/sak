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

/// Open a SQLite database file read-only.
///
/// Uses `SQLITE_OPEN_READ_ONLY | SQLITE_OPEN_URI` and then immediately runs
/// `PRAGMA query_only = ON;` as defense in depth. Returns an error if the
/// file does not exist, is not a SQLite database, or cannot be read.
//
// `#[allow(dead_code)]` until the first sqlite subcommand wires it in. The
// foundation issue intentionally adds no commands; dependent issues
// (`tables`, `schema`, `query`, ...) consume this helper.
#[allow(dead_code)]
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsStr;
    use std::fs;
    use std::path::PathBuf;

    /// Tokens that must not appear in any `src/sqlite/*.rs` file other than
    /// `client.rs`. Comments are exempt — the skip logic below ignores any
    /// line whose first non-whitespace characters are `//`.
    const FORBIDDEN_TOKENS: &[&str] = &[
        "rusqlite::Connection",
        "Connection::open",
        ".execute(",
        ".execute_batch(",
    ];

    #[test]
    fn no_mutation_methods_outside_client_module() {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/sqlite");
        let entries = fs::read_dir(&dir).expect("read src/sqlite");

        let mut violations = Vec::new();
        for entry in entries {
            let entry = entry.expect("dir entry");
            let path = entry.path();
            if path.extension() != Some(OsStr::new("rs")) {
                continue;
            }
            if path.file_name() == Some(OsStr::new("client.rs")) {
                continue;
            }

            let content = fs::read_to_string(&path).expect("read source file");
            for (idx, line) in content.lines().enumerate() {
                let trimmed = line.trim_start();
                // Skip line comments and doc comments — they're allowed to
                // mention forbidden tokens for documentation purposes.
                if trimmed.starts_with("//") {
                    continue;
                }
                for token in FORBIDDEN_TOKENS {
                    if line.contains(token) {
                        violations.push(format!(
                            "{}:{}: forbidden token `{}` outside client.rs",
                            path.display(),
                            idx + 1,
                            token
                        ));
                    }
                }
            }
        }

        assert!(
            violations.is_empty(),
            "rusqlite::Connection / mutation methods must be confined to src/sqlite/client.rs:\n{}",
            violations.join("\n")
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
