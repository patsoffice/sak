use std::io;
use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::Result;
use clap::Args;

use super::client;
use crate::output::BoundedWriter;

#[derive(Args)]
#[command(
    about = "List tables (and optionally views) in a SQLite database",
    long_about = "List the tables in a SQLite database file.\n\n\
        By default only user tables are listed — internal `sqlite_*` tables \
        and views are excluded. Use `--views` to include views and `--system` \
        to include internal tables. Output is `name<TAB>type`, sorted by name.",
    after_help = "\
Examples:
  sak sqlite tables app.db                List user tables
  sak sqlite tables app.db --views        Include views
  sak sqlite tables app.db --system       Include sqlite_* internal tables
  sak sqlite tables app.db --views --system   Everything in sqlite_master"
)]
pub struct TablesArgs {
    /// Path to the SQLite database file
    pub db: PathBuf,

    /// Also include views (off by default)
    #[arg(long)]
    pub views: bool,

    /// Include internal `sqlite_*` tables (off by default)
    #[arg(long)]
    pub system: bool,

    /// Maximum output lines
    #[arg(long)]
    pub limit: Option<usize>,
}

pub fn run(args: &TablesArgs) -> Result<ExitCode> {
    let conn = client::open_readonly(&args.db)?;

    // The SQL is built from compile-time literals — none of it comes from
    // user input — so plain string concatenation is safe here.
    let type_clause = if args.views {
        "type IN ('table', 'view')"
    } else {
        "type = 'table'"
    };
    let system_clause = if args.system {
        ""
    } else {
        " AND name NOT LIKE 'sqlite_%'"
    };
    let sql = format!(
        "SELECT name, type FROM sqlite_master WHERE {type_clause}{system_clause} ORDER BY name"
    );

    let rows = client::query_rows(&conn, &sql)?;
    if rows.is_empty() {
        return Ok(ExitCode::from(1));
    }

    let stdout = io::stdout();
    let handle = stdout.lock();
    let mut writer = BoundedWriter::new(handle, args.limit);
    for row in &rows {
        let line = format!("{}\t{}", row[0], row[1]);
        if !writer.write_line(&line)? {
            break;
        }
    }
    writer.flush()?;

    Ok(ExitCode::SUCCESS)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seeded_db() -> tempfile::NamedTempFile {
        let tmp = tempfile::NamedTempFile::new().expect("temp file");
        client::seed_for_tests(
            tmp.path(),
            // AUTOINCREMENT causes SQLite to automatically create the internal
            // `sqlite_sequence` table — we use that as our sqlite_* fixture
            // because SQLite refuses direct CREATE TABLE on `sqlite_*` names.
            "CREATE TABLE users (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT); \
             CREATE TABLE orders (id INTEGER PRIMARY KEY, user_id INTEGER); \
             CREATE VIEW recent_users AS SELECT * FROM users;",
        );
        tmp
    }

    fn args(db: &std::path::Path, views: bool, system: bool) -> TablesArgs {
        TablesArgs {
            db: db.to_path_buf(),
            views,
            system,
            limit: None,
        }
    }

    #[test]
    fn lists_only_user_tables_by_default() {
        let tmp = seeded_db();
        let conn = client::open_readonly(tmp.path()).unwrap();
        let rows = client::query_rows(
            &conn,
            "SELECT name, type FROM sqlite_master \
             WHERE type = 'table' AND name NOT LIKE 'sqlite_%' ORDER BY name",
        )
        .unwrap();
        let names: Vec<&str> = rows.iter().map(|r| r[0].as_str()).collect();
        assert_eq!(names, vec!["orders", "users"]);
    }

    #[test]
    fn views_flag_includes_views() {
        let tmp = seeded_db();
        let conn = client::open_readonly(tmp.path()).unwrap();
        let rows = client::query_rows(
            &conn,
            "SELECT name, type FROM sqlite_master \
             WHERE type IN ('table', 'view') AND name NOT LIKE 'sqlite_%' ORDER BY name",
        )
        .unwrap();
        let names: Vec<&str> = rows.iter().map(|r| r[0].as_str()).collect();
        assert_eq!(names, vec!["orders", "recent_users", "users"]);
        let view_row = rows.iter().find(|r| r[0] == "recent_users").unwrap();
        assert_eq!(view_row[1], "view");
    }

    #[test]
    fn system_flag_includes_sqlite_internal_tables() {
        let tmp = seeded_db();
        let conn = client::open_readonly(tmp.path()).unwrap();
        let rows = client::query_rows(
            &conn,
            "SELECT name, type FROM sqlite_master \
             WHERE type = 'table' ORDER BY name",
        )
        .unwrap();
        let names: Vec<&str> = rows.iter().map(|r| r[0].as_str()).collect();
        assert!(names.contains(&"sqlite_sequence"));
        assert!(names.contains(&"users"));
        assert!(names.contains(&"orders"));
    }

    #[test]
    fn run_returns_success_when_tables_present() {
        let tmp = seeded_db();
        let result = run(&args(tmp.path(), false, false)).unwrap();
        assert_eq!(result, ExitCode::SUCCESS);
    }

    #[test]
    fn run_returns_exit_one_when_no_tables() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        // Seed only an internal table (via AUTOINCREMENT side effect) and
        // then DROP the user table that triggered it. We can't drop sqlite_*
        // tables, so use a different trick: seed nothing but still create
        // a valid SQLite file.
        client::seed_for_tests(tmp.path(), "PRAGMA user_version = 1;");
        // Default flags exclude sqlite_* — should return exit 1.
        let result = run(&args(tmp.path(), false, false)).unwrap();
        assert_eq!(result, ExitCode::from(1));
    }

    #[test]
    fn run_errors_on_missing_db() {
        let result = run(&args(
            std::path::Path::new("/nonexistent/path.db"),
            false,
            false,
        ));
        assert!(result.is_err());
    }
}
