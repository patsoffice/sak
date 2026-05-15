use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::Result;
use clap::Args;

use crate::cert::{CertInfo, FIELD_NAMES, OutputFormat, inspect, parse_cert, read_cert_inputs};

#[derive(Args)]
#[command(
    about = "List certificates expiring within a window",
    long_about = "List certificates whose `notAfter` is less than --days from now.\n\n\
        Same input handling as `sak cert inspect` — PEM, DER, base64-wrapped \
        PEM, single cert or bundle, files or stdin. Already-expired certs \
        always match (their days_remaining is negative).\n\n\
        Exit code is inverted from `sak cert inspect` for shell ergonomics:\n\
        \n  • Exit 0 — no certs match (everything is healthy)\n  • Exit 1 — at \
        least one cert matches (drives `if sak cert expiring; then alert; fi`)\n  • \
        Exit 2 — error\n\nThis matches `grep`'s convention where success means \
        \"nothing to report.\"",
    after_help = "\
Examples:
  sak cert expiring cert.pem                          Default 30-day window
  sak cert expiring --days 90 *.pem                   90-day window
  sak cert expiring --days 7 chain.pem && echo OK     Healthy if no output
  sak cert expiring --tsv --days 60 *.pem             TSV for spreadsheets
  for f in /etc/pki/**/*.pem; do                      Sweep a directory tree
    sak cert expiring --field source --days 30 \"$f\"
  done"
)]
pub struct ExpiringArgs {
    /// Input files (reads stdin if omitted)
    pub files: Vec<PathBuf>,

    /// Match certs with fewer than N days until notAfter
    #[arg(long, default_value_t = 30)]
    pub days: i64,

    /// Output format (default: kv)
    #[arg(long, value_enum, default_value_t = OutputFormat::Kv, conflicts_with = "field")]
    pub format: OutputFormat,

    /// Convenience for --format json
    #[arg(long, conflicts_with_all = ["tsv", "field", "format"])]
    pub json: bool,

    /// Convenience for --format tsv
    #[arg(long, conflicts_with_all = ["json", "field", "format"])]
    pub tsv: bool,

    /// Print only this field, one value per matching cert
    #[arg(long, value_name = "NAME")]
    pub field: Option<String>,

    /// Maximum number of output lines
    #[arg(long)]
    pub limit: Option<usize>,
}

pub fn run(args: &ExpiringArgs) -> Result<ExitCode> {
    let format = if args.json {
        OutputFormat::Json
    } else if args.tsv {
        OutputFormat::Tsv
    } else {
        args.format
    };

    if let Some(field) = &args.field
        && !FIELD_NAMES.contains(&field.as_str())
    {
        anyhow::bail!(
            "unknown --field `{}` (valid: {})",
            field,
            FIELD_NAMES.join(", ")
        );
    }

    let raw_inputs = read_cert_inputs(&args.files)?;

    let mut infos: Vec<CertInfo> = Vec::new();
    let mut prev_source: Option<String> = None;
    let mut next_index = 0usize;
    for (source, der) in &raw_inputs {
        if prev_source.as_deref() != Some(source.as_str()) {
            next_index = 0;
            prev_source = Some(source.clone());
        }
        let info = parse_cert(source, next_index, der, "")?;
        next_index += 1;
        if info.days_remaining < args.days {
            infos.push(info);
        }
    }

    let any_matched = !infos.is_empty();
    inspect::emit(&infos, format, args.field.as_deref(), args.limit)?;

    // Spec inversion: exit 1 means "found something to alert on", which is
    // the opposite of sak's normal "exit 0 = found, exit 1 = nothing".
    // Documented prominently in long_about so callers aren't surprised.
    if any_matched {
        Ok(ExitCode::from(1))
    } else {
        Ok(ExitCode::SUCCESS)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_tmp(content: &str) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("c.pem");
        let mut f = std::fs::File::create(&p).unwrap();
        f.write_all(content.as_bytes()).unwrap();
        (dir, p)
    }

    #[test]
    fn expiring_short_window_no_match() {
        // sak-test cert is valid through 2036, so a 7-day window matches
        // nothing → exit 0.
        let (_d, p) = write_tmp(crate::cert::tests::TEST_PEM);
        let args = ExpiringArgs {
            files: vec![p],
            days: 7,
            format: OutputFormat::Kv,
            json: false,
            tsv: false,
            field: None,
            limit: None,
        };
        assert_eq!(run(&args).unwrap(), ExitCode::SUCCESS);
    }

    #[test]
    fn expiring_huge_window_matches() {
        // 100_000 days ≫ time-until-2036, so this should always match → exit 1.
        let (_d, p) = write_tmp(crate::cert::tests::TEST_PEM);
        let args = ExpiringArgs {
            files: vec![p],
            days: 100_000,
            format: OutputFormat::Kv,
            json: false,
            tsv: false,
            field: None,
            limit: None,
        };
        assert_eq!(run(&args).unwrap(), ExitCode::from(1));
    }
}
