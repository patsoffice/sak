pub mod headers;
pub mod query;
pub mod stats;
pub mod validate;

use crate::output::Outcome;

use std::io::{self, BufRead, BufReader};
use std::path::Path;

use anyhow::{Context, Result};
use clap::Subcommand;

/// True if a file argument is the conventional "-" stdin sentinel (cat, jq,
/// grep, ...). A path of "-" reads stdin; csv needs no format hint since the
/// delimiter is flag-driven, not extension-sniffed.
pub(crate) fn is_stdin(path: &Path) -> bool {
    path.as_os_str() == "-"
}

/// Resolve a file argument to a `(display_name, buffered reader)` pair, reading
/// from stdin (named `<stdin>`) when the argument is the "-" sentinel. This is
/// the csv domain's read chokepoint — the equivalent of json's `read_source` /
/// config's `read_config_value` — so every command honors "-" uniformly.
pub(crate) fn open_reader(path: &Path) -> Result<(String, Box<dyn BufRead>)> {
    if is_stdin(path) {
        Ok(("<stdin>".to_string(), Box::new(io::stdin().lock())))
    } else {
        let file = std::fs::File::open(path)
            .with_context(|| format!("cannot open: {}", path.display()))?;
        Ok((path.display().to_string(), Box::new(BufReader::new(file))))
    }
}

#[derive(Subcommand)]
pub enum CsvCommand {
    Headers(headers::HeadersArgs),
    Query(query::QueryArgs),
    Stats(stats::StatsArgs),
    Validate(validate::ValidateArgs),
}

pub fn run(cmd: &CsvCommand) -> Result<Outcome> {
    match cmd {
        CsvCommand::Headers(args) => headers::run(args),
        CsvCommand::Query(args) => query::run(args),
        CsvCommand::Stats(args) => stats::run(args),
        CsvCommand::Validate(args) => validate::run(args),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    #[test]
    fn dash_is_stdin_sentinel() {
        assert!(is_stdin(Path::new("-")));
    }

    #[test]
    fn ordinary_paths_are_not_stdin() {
        assert!(!is_stdin(Path::new("a.csv")));
        assert!(!is_stdin(Path::new("./-")));
        assert!(!is_stdin(Path::new("-.csv")));
        assert!(!is_stdin(Path::new("--")));
    }

    #[test]
    fn open_reader_reads_named_file() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.csv");
        std::fs::write(&p, "name,age\nalice,30\n").unwrap();
        let (name, mut reader) = open_reader(&p).unwrap();
        assert_eq!(name, p.display().to_string());
        let mut s = String::new();
        reader.read_to_string(&mut s).unwrap();
        assert_eq!(s, "name,age\nalice,30\n");
    }

    #[test]
    fn open_reader_missing_file_errors() {
        let err = open_reader(Path::new("/no/such/file.csv"))
            .err()
            .expect("missing file should error");
        assert!(err.to_string().contains("cannot open"));
    }
}
