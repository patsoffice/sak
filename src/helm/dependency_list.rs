use crate::output::Outcome;

use anyhow::Result;
use clap::Args;
use serde::Serialize;

use crate::helm::Format;
use crate::helm::client::{self, Conn};
use crate::output::BoundedWriter;

/// Fixed TSV column set / JSON field order.
const COLUMNS: [&str; 4] = ["name", "version", "repository", "status"];

/// One declared chart dependency, as parsed from `helm dependency list`'s
/// table. `status` is helm's check result (`ok` / `missing` / `unpacked` /
/// `wrong version` / ...).
#[derive(Debug, PartialEq, Eq, Serialize)]
pub struct Dep {
    pub name: String,
    pub version: String,
    pub repository: String,
    pub status: String,
}

#[derive(Args)]
#[command(
    about = "List a chart's declared dependencies as TSV (read-only)",
    long_about = "List a chart's declared dependencies via `helm dependency list <chart>`, \
        one TSV row per dependency with the columns \
        name, version, repository, status.\n\n\
        `<chart>` is a path to a local chart directory or a packaged `.tgz`; \
        dependencies come from the chart's `Chart.yaml` (or legacy \
        `requirements.yaml`). `status` is helm's check result — `ok`, \
        `missing`, `unpacked`, or `wrong version`. `helm` does not emit JSON \
        for this verb, so `sak` parses the aligned table; `--format json` \
        re-serializes the parsed rows as a JSON array.\n\n\
        This reads local chart files only — no registry or cluster contact, no \
        downloads (run `helm dependency build` separately for that).\n\n\
        Exit status: 0 when the chart declares dependencies, 1 when it declares \
        none, 2 on error (e.g. a missing chart path).",
    after_help = "\
Examples:
  sak helm dependency-list ./mychart              Dependencies as TSV
  sak helm dependency-list ./mychart --format json
  sak helm dependency-list /charts/app-1.2.3.tgz"
)]
pub struct DependencyListArgs {
    /// Path to a chart directory or packaged `.tgz`
    #[arg(value_name = "CHART")]
    pub chart: String,

    /// Output format
    #[arg(long, value_enum, default_value_t = Format::Tsv)]
    pub format: Format,

    /// Maximum number of output lines (bounds output, not the helm fetch)
    #[arg(long)]
    pub limit: Option<usize>,
}

pub fn run(args: &DependencyListArgs) -> Result<Outcome> {
    // `helm dependency list` reads local chart files only — no cluster.
    let stdout = client::invoke_ok("dependency", Some("list"), &[&args.chart], Conn::default())?;
    let text = String::from_utf8_lossy(&stdout);
    let deps = parse(&text);

    let out = std::io::stdout();
    let mut writer = BoundedWriter::new(out.lock(), args.limit);
    let any = match args.format {
        Format::Tsv => emit_tsv(&mut writer, &deps)?,
        Format::Json => emit_json(&mut writer, &deps)?,
    };
    writer.flush()?;
    Ok(if any {
        Outcome::Found
    } else {
        Outcome::NotFound
    })
}

/// Parse `helm dependency list`'s table. Despite looking space-aligned, `helm`
/// renders it with `text/tabwriter` using a real `\t` between columns (the
/// cells are space-padded to align on a terminal, then tab-separated). So the
/// robust split is on `\t`, trimming each field's alignment padding. This is
/// immune to values that contain spaces (the two-word `wrong version` status,
/// which has no tab) and to an empty `repository` cell (a `\t\t` run yields an
/// empty field) — both of which a whitespace tokenizer would mis-handle.
///
/// Pure over its input so it's testable on hand-built fixtures.
pub fn parse(output: &str) -> Vec<Dep> {
    let mut lines = output.lines().filter(|l| !l.trim().is_empty());

    // Skip to the header row carrying the column labels; anything before it
    // (e.g. a `WARNING:` line) is ignored. Data rows follow.
    if lines
        .find(|l| l.contains("NAME") && l.contains("STATUS"))
        .is_none()
    {
        return Vec::new();
    }

    lines
        .filter_map(|line| {
            let mut fields = line.split('\t').map(str::trim);
            let name = fields.next().unwrap_or("");
            if name.is_empty() {
                return None;
            }
            Some(Dep {
                name: name.to_string(),
                version: fields.next().unwrap_or("").to_string(),
                repository: fields.next().unwrap_or("").to_string(),
                status: fields.next().unwrap_or("").to_string(),
            })
        })
        .collect()
}

fn emit_tsv(writer: &mut BoundedWriter<'_>, deps: &[Dep]) -> Result<bool> {
    if deps.is_empty() {
        return Ok(false);
    }
    writer.write_decoration(&COLUMNS.join("\t"))?;
    for d in deps {
        let row = format!("{}\t{}\t{}\t{}", d.name, d.version, d.repository, d.status);
        if !writer.write_line(&row)? {
            break;
        }
    }
    Ok(true)
}

/// `helm` has no JSON output for this verb, so emit the parsed rows as a
/// compact JSON array. Empty (no dependencies) writes nothing and is exit 1.
fn emit_json(writer: &mut BoundedWriter<'_>, deps: &[Dep]) -> Result<bool> {
    if deps.is_empty() {
        return Ok(false);
    }
    writer.write_line(&serde_json::to_string(deps)?)?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    // A representative `helm dependency list` table. Columns are TAB-separated
    // with space padding for terminal alignment (exactly how helm's tabwriter
    // emits it — see the `\t`s). Covers a two-word status and an empty
    // repository cell (`\t\t`).
    const TABLE: &str = "\
NAME      \tVERSION\tREPOSITORY                        \tSTATUS \n\
common    \t2.x.x  \thttps://charts.bitnami.com/bitnami\tok\n\
mariadb   \t11.x.x \thttps://charts.bitnami.com/bitnami\tmissing\n\
postgresql\t12.x.x \thttps://charts.bitnami.com/bitnami\twrong version\n\
localdep  \t0.1.0  \t                                  \tunpacked\n";

    #[test]
    fn parse_extracts_all_columns() {
        let deps = parse(TABLE);
        assert_eq!(deps.len(), 4);
        assert_eq!(
            deps[0],
            Dep {
                name: "common".into(),
                version: "2.x.x".into(),
                repository: "https://charts.bitnami.com/bitnami".into(),
                status: "ok".into(),
            }
        );
    }

    #[test]
    fn parse_keeps_two_word_status() {
        let deps = parse(TABLE);
        assert_eq!(deps[2].name, "postgresql");
        assert_eq!(deps[2].status, "wrong version");
    }

    #[test]
    fn parse_handles_empty_repository() {
        let deps = parse(TABLE);
        assert_eq!(deps[3].name, "localdep");
        assert_eq!(deps[3].repository, "");
        assert_eq!(deps[3].status, "unpacked");
    }

    #[test]
    fn parse_skips_leading_warning_line() {
        let with_warning = format!("WARNING: bad thing\n{TABLE}");
        assert_eq!(parse(&with_warning).len(), 4);
    }

    #[test]
    fn parse_no_table_yields_nothing() {
        assert!(parse("").is_empty());
        assert!(parse("WARNING: no dependencies\n").is_empty());
    }

    #[test]
    fn parse_header_only_yields_nothing() {
        assert!(parse("NAME VERSION REPOSITORY STATUS\n").is_empty());
    }

    #[test]
    fn json_serializes_in_column_order() {
        let deps = parse(TABLE);
        let json = serde_json::to_string(&deps[0]).unwrap();
        assert_eq!(
            json,
            r#"{"name":"common","version":"2.x.x","repository":"https://charts.bitnami.com/bitnami","status":"ok"}"#
        );
    }
}
