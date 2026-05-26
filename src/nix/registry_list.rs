use std::process::ExitCode;

use anyhow::Result;
use clap::{Args, ValueEnum};
use serde::Serialize;

use crate::nix::Format;
use crate::nix::client;
use crate::output::BoundedWriter;

/// Registry scope, used both as the `--scope` filter and the value of the
/// `scope` column. `nix registry list` prefixes each line with one of these.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum Scope {
    User,
    System,
    Global,
    Flag,
}

impl Scope {
    fn as_str(self) -> &'static str {
        match self {
            Scope::User => "user",
            Scope::System => "system",
            Scope::Global => "global",
            Scope::Flag => "flag",
        }
    }
}

#[derive(Args)]
#[command(
    about = "List Nix flake registry entries as TSV (read-only)",
    long_about = "List the Nix flake registry via `nix registry list` and emit one TSV row \
        per entry with the columns scope, from, to.\n\n\
        Each entry maps a short flake reference (`from`, e.g. `flake:nixpkgs`) to \
        a concrete location (`to`, e.g. `github:NixOS/nixpkgs` or a `path:` store \
        path). `scope` is one of user / system / global / flag (the registry is \
        layered; `nix` merges them with user winning over global). `nix registry \
        list` has no `--json` form, so sak parses its whitespace-separated text \
        output; `--format json` re-serializes the parsed rows as a JSON array for \
        piping into `sak json`.\n\n\
        Use `--scope <user|system|global|flag>` to show only one layer.\n\n\
        Exit status: 0 when at least one entry is listed, 1 when none (an empty \
        registry, or a `--scope` that matches nothing), 2 on error.",
    after_help = "\
Examples:
  sak nix registry-list                        All registry entries as TSV
  sak nix registry-list --scope user           Only user-pinned entries
  sak nix registry-list --format json          Parsed entries as a JSON array"
)]
pub struct RegistryListArgs {
    /// Show only entries in this scope
    #[arg(long, value_enum, value_name = "SCOPE")]
    pub scope: Option<Scope>,

    /// Output format
    #[arg(long, value_enum, default_value_t = Format::Tsv)]
    pub format: Format,

    /// Maximum number of output lines (bounds output, not the nix call)
    #[arg(long)]
    pub limit: Option<usize>,
}

/// Fixed TSV column set, in emission order.
const COLUMNS: [&str; 3] = ["scope", "from", "to"];

/// One registry entry.
#[derive(Debug, PartialEq, Eq, Serialize)]
pub struct Row {
    pub scope: String,
    pub from: String,
    pub to: String,
}

impl Row {
    fn to_tsv(&self) -> String {
        format!("{}\t{}\t{}", self.scope, self.from, self.to)
    }
}

pub fn run(args: &RegistryListArgs) -> Result<ExitCode> {
    let stdout = client::invoke_ok("registry", Some("list"), &[])?;
    let text = String::from_utf8_lossy(&stdout);
    let rows: Vec<Row> = parse(&text)
        .into_iter()
        .filter(|r| args.scope.is_none_or(|s| r.scope == s.as_str()))
        .collect();

    if rows.is_empty() {
        return Ok(ExitCode::from(1));
    }

    let out = std::io::stdout();
    let mut writer = BoundedWriter::new(out.lock(), args.limit);
    match args.format {
        Format::Json => {
            writer.write_line(&serde_json::to_string(&rows)?)?;
        }
        Format::Tsv => {
            writer.write_decoration(&COLUMNS.join("\t"))?;
            for row in &rows {
                if !writer.write_line(&row.to_tsv())? {
                    break;
                }
            }
        }
    }
    writer.flush()?;
    Ok(ExitCode::SUCCESS)
}

/// Parse `nix registry list` text into rows. Each non-blank line is
/// `<scope> <from> <to>` separated by whitespace; the first two tokens are
/// scope and from, and the remainder (joined on a single space) is `to` so a
/// target with embedded spaces still round-trips. Lines with fewer than three
/// tokens are skipped. Pure over its input so it's testable on a hand-built
/// fixture.
pub fn parse(text: &str) -> Vec<Row> {
    text.lines()
        .filter_map(|line| {
            let mut parts = line.split_whitespace();
            let scope = parts.next()?;
            let from = parts.next()?;
            let to = parts.collect::<Vec<_>>().join(" ");
            if to.is_empty() {
                return None;
            }
            Some(Row {
                scope: scope.to_string(),
                from: from.to_string(),
                to,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = "\
system flake:nixpkgs path:/nix/store/abc-source
global flake:agda github:agda/agda
user flake:mine github:me/mine
";

    #[test]
    fn parse_splits_three_columns() {
        let rows = parse(FIXTURE);
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].scope, "system");
        assert_eq!(rows[0].from, "flake:nixpkgs");
        assert_eq!(rows[0].to, "path:/nix/store/abc-source");
        assert_eq!(rows[1].scope, "global");
        assert_eq!(rows[1].to, "github:agda/agda");
    }

    #[test]
    fn parse_skips_short_and_blank_lines() {
        // Blank / whitespace-only lines and 2-token lines (no `to`) are dropped.
        let rows = parse("global flake:x github:o/x\n\nuser flake:y\n   \n");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].from, "flake:x");
    }

    #[test]
    fn parse_drops_two_token_line() {
        // Only scope + from, no target -> not an entry.
        assert!(parse("global flake:x\n").is_empty());
    }

    #[test]
    fn row_to_tsv_is_tab_separated() {
        let row = Row {
            scope: "global".to_string(),
            from: "flake:agda".to_string(),
            to: "github:agda/agda".to_string(),
        };
        assert_eq!(row.to_tsv(), "global\tflake:agda\tgithub:agda/agda");
    }

    #[test]
    fn scope_as_str_round_trips() {
        assert_eq!(Scope::User.as_str(), "user");
        assert_eq!(Scope::System.as_str(), "system");
        assert_eq!(Scope::Global.as_str(), "global");
        assert_eq!(Scope::Flag.as_str(), "flag");
    }
}
