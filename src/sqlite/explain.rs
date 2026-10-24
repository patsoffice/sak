use crate::output::Outcome;
use std::io;
use std::path::PathBuf;

use anyhow::{Result, bail};
use clap::Args;

use super::client;
use super::query::{OutputFormat, row_to_json_line, row_to_tsv_line};
use crate::output::BoundedWriter;

#[derive(Args)]
#[command(
    about = "Show SQLite's query plan for a statement",
    long_about = "Run `EXPLAIN QUERY PLAN <sql>` against a SQLite database \
        file and show how the engine executes the statement — which index \
        (if any) each table access uses. The plan of last resort when a \
        query is slow.\n\n\
        The statement goes through the same read-only gate as `sak sqlite \
        query` (SELECT or WITH; a leading `EXPLAIN` or `EXPLAIN QUERY PLAN` \
        is dropped, and PRAGMA is refused since it has no plan), then runs \
        wrapped on the read-only connection. `--bytecode` runs plain `EXPLAIN <sql>` \
        instead — the lower-level VDBE program (addr/opcode/p1..p5/comment \
        columns) rather than the query-plan summary \
        (id/parent/notused/detail). Column names always come from the \
        cursor, so output tracks whatever SQLite version runs the query.",
    after_help = "\
Examples:
  sak sqlite explain app.db 'SELECT * FROM users WHERE email = ?'
  sak sqlite explain app.db 'SELECT * FROM orders' --format json
  sak sqlite explain app.db 'SELECT * FROM users' --bytecode"
)]
pub struct ExplainArgs {
    /// Path to the SQLite database file
    pub db: PathBuf,

    /// SQL statement to plan (SELECT or WITH; a leading EXPLAIN is ignored)
    pub sql: String,

    /// Show the VDBE bytecode program instead of the query-plan summary
    #[arg(long)]
    pub bytecode: bool,

    /// Output format
    #[arg(long, value_enum, default_value_t = OutputFormat::Tsv)]
    pub format: OutputFormat,

    /// Maximum number of plan rows to emit
    #[arg(long)]
    pub limit: Option<usize>,
}

/// Wrap the statement and collect the plan as typed rows. The cursor's own
/// column names are returned alongside the rows: `id`, `parent`, `notused`,
/// `detail` for `EXPLAIN QUERY PLAN`; `addr`, `opcode`, `p1`…`p5`,
/// `comment` for `--bytecode`. Separated from [`run`] so tests can assert
/// plan content instead of only the exit [`Outcome`].
pub(crate) fn plan_rows(
    conn: &client::Conn,
    sql: &str,
    bytecode: bool,
) -> Result<(Vec<String>, Vec<Vec<rusqlite::types::Value>>)> {
    let sql = plan_target(sql)?;
    let wrapped = if bytecode {
        format!("EXPLAIN {sql}")
    } else {
        format!("EXPLAIN QUERY PLAN {sql}")
    };
    client::query_rows_typed(conn, &wrapped)
}

/// The statement to plan. A leading `EXPLAIN` or `EXPLAIN QUERY PLAN` is
/// dropped, since the wrapper adds its own and SQLite rejects a doubled one.
/// `PRAGMA` passes the read gate but has no query plan, so it's refused
/// rather than answered with an empty plan.
fn plan_target(sql: &str) -> Result<&str> {
    use super::query::{strip_keyword, strip_leading_comments_and_ws as skip_ws};
    let mut body = skip_ws(sql);
    if let Some(rest) = strip_keyword(body, "explain") {
        body = skip_ws(rest);
        if let Some(rest) = strip_keyword(body, "query").map(skip_ws)
            && let Some(rest) = strip_keyword(rest, "plan")
        {
            body = skip_ws(rest);
        }
    }
    if strip_keyword(body, "pragma").is_some() {
        bail!("PRAGMA statements have no query plan; run them with `sak sqlite query`");
    }
    super::query::ensure_read_statement(body)?;
    Ok(body)
}

pub fn run(args: &ExplainArgs) -> Result<Outcome> {
    let conn = client::open_readonly(&args.db)?;
    let (columns, rows) = plan_rows(&conn, &args.sql, args.bytecode)?;

    let stdout = io::stdout();
    let handle = stdout.lock();
    let mut writer = BoundedWriter::new(handle, args.limit);

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
        Ok(Outcome::NotFound)
    } else {
        Ok(Outcome::Found)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::types::Value;

    fn seeded_db() -> tempfile::NamedTempFile {
        let tmp = tempfile::NamedTempFile::new().expect("temp file");
        client::seed_for_tests(
            tmp.path(),
            "CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT, email TEXT); \
             CREATE INDEX idx_users_email ON users(email); \
             CREATE TABLE orders (id INTEGER PRIMARY KEY, item TEXT); \
             INSERT INTO users VALUES (1, 'alice', 'a@x'); \
             INSERT INTO orders VALUES (1, 'book');",
        );
        tmp
    }

    fn plan(db: &std::path::Path, sql: &str, bytecode: bool) -> (Vec<String>, Vec<Vec<Value>>) {
        let conn = client::open_readonly(db).unwrap();
        plan_rows(&conn, sql, bytecode).unwrap()
    }

    fn details(rows: &[Vec<Value>]) -> Vec<String> {
        rows.iter()
            .map(|row| match &row[3] {
                Value::Text(t) => t.clone(),
                other => panic!("expected TEXT detail, got {other:?}"),
            })
            .collect()
    }

    #[test]
    fn eqp_uses_index_for_indexed_lookup() {
        let tmp = seeded_db();
        let (cols, rows) = plan(tmp.path(), "SELECT * FROM users WHERE email = 'a@x'", false);
        assert_eq!(cols, vec!["id", "parent", "notused", "detail"]);
        let details = details(&rows);
        assert!(
            details
                .iter()
                .any(|d| d.contains("SEARCH") && d.contains("idx_users_email")),
            "{details:?}"
        );
    }

    #[test]
    fn eqp_scans_unindexed_table() {
        let tmp = seeded_db();
        let (_, rows) = plan(tmp.path(), "SELECT * FROM orders", false);
        let details = details(&rows);
        assert!(details.iter().any(|d| d.contains("SCAN")), "{details:?}");
    }

    #[test]
    fn bytecode_has_vdbe_columns() {
        let tmp = seeded_db();
        let (cols, rows) = plan(tmp.path(), "SELECT * FROM users", true);
        assert_eq!(
            cols,
            vec!["addr", "opcode", "p1", "p2", "p3", "p4", "p5", "comment"]
        );
        assert!(!rows.is_empty());
    }

    #[test]
    fn leading_explain_is_dropped_not_doubled() {
        // Regression: the gate accepted `EXPLAIN …`, then wrapping produced
        // `EXPLAIN QUERY PLAN EXPLAIN …`, a syntax error.
        let tmp = seeded_db();
        for sql in [
            "EXPLAIN QUERY PLAN SELECT * FROM users WHERE email = 'a@x'",
            "explain query plan SELECT * FROM users WHERE email = 'a@x'",
            "-- note\nEXPLAIN SELECT * FROM users WHERE email = 'a@x'",
        ] {
            let (cols, rows) = plan(tmp.path(), sql, false);
            assert_eq!(cols, vec!["id", "parent", "notused", "detail"], "{sql}");
            assert!(
                details(&rows).iter().any(|d| d.contains("idx_users_email")),
                "{sql}"
            );
        }
        let (cols, _) = plan(tmp.path(), "EXPLAIN SELECT * FROM users", true);
        assert_eq!(cols[1], "opcode");
    }

    #[test]
    fn plan_target_strips_and_gates() {
        assert_eq!(plan_target("SELECT 1").unwrap(), "SELECT 1");
        assert_eq!(plan_target("EXPLAIN  SELECT 1").unwrap(), "SELECT 1");
        assert_eq!(
            plan_target("EXPLAIN QUERY /* c */ PLAN WITH x AS (SELECT 1) SELECT * FROM x").unwrap(),
            "WITH x AS (SELECT 1) SELECT * FROM x"
        );
        // `explained` is an identifier, not the keyword.
        assert!(plan_target("explained").is_err());
        // An EXPLAIN-wrapped write is still a write.
        assert!(plan_target("EXPLAIN DELETE FROM users").is_err());
        assert!(plan_target("EXPLAIN").is_err());
    }

    #[test]
    fn pragma_is_refused_with_a_pointer_to_query() {
        // Regression: PRAGMA passed the gate and yielded an empty plan (exit 1).
        let tmp = seeded_db();
        let conn = client::open_readonly(tmp.path()).unwrap();
        for sql in [
            "PRAGMA table_info(users)",
            "EXPLAIN PRAGMA table_info(users)",
        ] {
            let err = plan_rows(&conn, sql, false).unwrap_err().to_string();
            assert!(err.contains("sak sqlite query"), "{sql}: {err}");
        }
    }

    #[test]
    fn run_emits_found_for_plan() {
        let tmp = seeded_db();
        let args = ExplainArgs {
            db: tmp.path().to_path_buf(),
            sql: "SELECT * FROM users WHERE email = 'a@x'".to_string(),
            bytecode: false,
            format: OutputFormat::Tsv,
            limit: None,
        };
        assert_eq!(run(&args).unwrap(), Outcome::Found);
    }

    #[test]
    fn run_rejects_mutation_statement() {
        let tmp = seeded_db();
        let args = ExplainArgs {
            db: tmp.path().to_path_buf(),
            sql: "DROP TABLE users".to_string(),
            bytecode: false,
            format: OutputFormat::Tsv,
            limit: None,
        };
        assert!(run(&args).is_err());
    }

    #[test]
    fn run_errors_on_missing_db() {
        let args = ExplainArgs {
            db: std::path::PathBuf::from("/nonexistent/path.db"),
            sql: "SELECT 1".to_string(),
            bytecode: false,
            format: OutputFormat::Tsv,
            limit: None,
        };
        assert!(run(&args).is_err());
    }
}
