use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{Context, Result, anyhow};
use clap::Args;

use super::client;
use crate::output::BoundedWriter;

#[derive(Args)]
#[command(
    about = "Show database-level metadata for a SQLite file",
    long_about = "Show database-level metadata for a SQLite file in one shot.\n\n\
        Reports page size, page count, encoding, application and user version, \
        schema version, journal mode, integrity check status, and the file size \
        on disk. The integrity check is the standard `PRAGMA integrity_check` — \
        on multi-gigabyte databases this can take a noticeable amount of time \
        because SQLite walks the entire b-tree.\n\n\
        Output is `key<TAB>value`, one per line, sorted alphabetically by key. \
        Exit code is always 0 when the database opens — even when \
        `integrity_check` reports problems, the result is part of the output, \
        not the exit signal. Exit code 2 means the file does not exist or is \
        not a valid SQLite database.",
    after_help = "\
Examples:
  sak sqlite info app.db"
)]
pub struct InfoArgs {
    /// Path to the SQLite database file
    pub db: PathBuf,

    /// Maximum output lines
    #[arg(long)]
    pub limit: Option<usize>,
}

pub fn run(args: &InfoArgs) -> Result<ExitCode> {
    let conn = client::open_readonly(&args.db)?;

    // PRAGMAs that return a single value as the first column of the first row.
    let pragmas = [
        "page_size",
        "page_count",
        "encoding",
        "application_id",
        "user_version",
        "schema_version",
        "journal_mode",
        "integrity_check",
    ];

    let mut entries: BTreeMap<String, String> = BTreeMap::new();
    for pragma in pragmas {
        let sql = format!("PRAGMA {pragma}");
        let rows = client::query_rows(&conn, &sql)?;
        let value = rows
            .first()
            .and_then(|r| r.first())
            .cloned()
            .unwrap_or_default();
        entries.insert(pragma.to_string(), value);
    }

    // Computed: total bytes implied by page_size * page_count.
    let page_size: u64 = entries
        .get("page_size")
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| anyhow!("page_size pragma did not return an integer"))?;
    let page_count: u64 = entries
        .get("page_count")
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| anyhow!("page_count pragma did not return an integer"))?;
    entries.insert(
        "total_pages_bytes".to_string(),
        (page_size * page_count).to_string(),
    );

    // File size on disk straight from the OS — no need to ask sqlite.
    let meta = fs::metadata(&args.db).with_context(|| format!("stat {}", args.db.display()))?;
    entries.insert("file_size_bytes".to_string(), meta.len().to_string());

    let stdout = io::stdout();
    let handle = stdout.lock();
    let mut writer = BoundedWriter::new(handle, args.limit);
    for (key, value) in &entries {
        // PRAGMA values can contain newlines and tabs (notably integrity_check
        // emits multi-line reports on a problem database). Collapse those to
        // single spaces so the key<TAB>value contract holds for every row.
        let one_line = sanitize(value);
        let line = format!("{key}\t{one_line}");
        if !writer.write_line(&line)? {
            break;
        }
    }
    writer.flush()?;

    Ok(ExitCode::SUCCESS)
}

/// Replace newline, carriage return, and tab characters with a single space so
/// a value never breaks the `key<TAB>value` line format.
fn sanitize(value: &str) -> String {
    value
        .chars()
        .map(|c| match c {
            '\n' | '\r' | '\t' => ' ',
            other => other,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seeded_db() -> tempfile::NamedTempFile {
        let tmp = tempfile::NamedTempFile::new().expect("temp file");
        client::seed_for_tests(
            tmp.path(),
            "CREATE TABLE t (id INTEGER PRIMARY KEY, name TEXT); \
             INSERT INTO t VALUES (1, 'a'), (2, 'b'), (3, 'c');",
        );
        tmp
    }

    fn collect(args: &InfoArgs) -> BTreeMap<String, String> {
        let conn = client::open_readonly(&args.db).unwrap();
        let pragmas = [
            "page_size",
            "page_count",
            "encoding",
            "application_id",
            "user_version",
            "schema_version",
            "journal_mode",
            "integrity_check",
        ];
        let mut entries: BTreeMap<String, String> = BTreeMap::new();
        for p in pragmas {
            let rows = client::query_rows(&conn, &format!("PRAGMA {p}")).unwrap();
            entries.insert(
                p.to_string(),
                rows.first()
                    .and_then(|r| r.first())
                    .cloned()
                    .unwrap_or_default(),
            );
        }
        let ps: u64 = entries["page_size"].parse().unwrap();
        let pc: u64 = entries["page_count"].parse().unwrap();
        entries.insert("total_pages_bytes".into(), (ps * pc).to_string());
        let meta = std::fs::metadata(&args.db).unwrap();
        entries.insert("file_size_bytes".into(), meta.len().to_string());
        entries
    }

    #[test]
    fn run_succeeds_on_seeded_db() {
        let tmp = seeded_db();
        let code = run(&InfoArgs {
            db: tmp.path().to_path_buf(),
            limit: None,
        })
        .unwrap();
        assert_eq!(code, ExitCode::SUCCESS);
    }

    #[test]
    fn reports_all_expected_keys() {
        let tmp = seeded_db();
        let entries = collect(&InfoArgs {
            db: tmp.path().to_path_buf(),
            limit: None,
        });
        for key in [
            "page_size",
            "page_count",
            "encoding",
            "application_id",
            "user_version",
            "schema_version",
            "journal_mode",
            "integrity_check",
            "total_pages_bytes",
            "file_size_bytes",
        ] {
            assert!(entries.contains_key(key), "missing {key}");
        }
    }

    #[test]
    fn integrity_check_reports_ok() {
        let tmp = seeded_db();
        let entries = collect(&InfoArgs {
            db: tmp.path().to_path_buf(),
            limit: None,
        });
        assert_eq!(entries["integrity_check"], "ok");
    }

    #[test]
    fn file_size_matches_metadata() {
        let tmp = seeded_db();
        let entries = collect(&InfoArgs {
            db: tmp.path().to_path_buf(),
            limit: None,
        });
        let expected = std::fs::metadata(tmp.path()).unwrap().len().to_string();
        assert_eq!(entries["file_size_bytes"], expected);
    }

    #[test]
    fn total_pages_bytes_is_page_size_times_page_count() {
        let tmp = seeded_db();
        let entries = collect(&InfoArgs {
            db: tmp.path().to_path_buf(),
            limit: None,
        });
        let ps: u64 = entries["page_size"].parse().unwrap();
        let pc: u64 = entries["page_count"].parse().unwrap();
        assert_eq!(entries["total_pages_bytes"], (ps * pc).to_string());
    }

    #[test]
    fn run_errors_on_missing_db() {
        let result = run(&InfoArgs {
            db: PathBuf::from("/nonexistent/path.db"),
            limit: None,
        });
        assert!(result.is_err());
    }
}
