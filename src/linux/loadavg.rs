//! `sak linux loadavg` — parse the single line of `/proc/loadavg`.
//!
//! `/proc/loadavg` is one line: `3.14 2.71 1.41 1/284 12345` — the 1/5/15-minute
//! load averages, a `running/total` schedulable-entity count, and the last PID
//! the kernel handed out. The value here over `cat /proc/loadavg | awk ...` is
//! consistent typed output: fixed columns in TSV, and real numbers (not strings)
//! in JSON, with the `running/total` pair split into its own fields.

use crate::output::Outcome;
use std::io;

use anyhow::Result;
use clap::Args;
use serde_json::{Value, json};

use super::{json_num, read_proc_file};
use crate::output::BoundedWriter;

#[derive(Args)]
#[command(
    about = "Parse /proc/loadavg into typed load + process fields",
    long_about = "Parse the single line of /proc/loadavg.\n\n\
        Default output is one TSV row:\n\n  \
        1m<TAB>5m<TAB>15m<TAB>running<TAB>total<TAB>last_pid\n\n\
        The first three columns are the 1-, 5-, and 15-minute load averages; \
        `running` and `total` are the kernel's `running/total` schedulable-entity \
        count split into two fields; `last_pid` is the most recently created PID.\n\n\
        `--format json` emits a single JSON object with the loads as numbers and \
        the counts as integers, so consumers get typed values rather than having \
        to re-parse the `3.14 2.71 1.41 1/284 12345` shell string.",
    after_help = "\
Examples:
  sak linux loadavg                  TSV: 1m 5m 15m running total last_pid
  sak linux loadavg --format json    Typed JSON object"
)]
pub struct LoadavgArgs {
    /// Output format
    #[arg(long, value_enum, default_value_t = Format::Tsv)]
    pub format: Format,
}

#[derive(Clone, Copy, clap::ValueEnum)]
pub enum Format {
    /// Tab-separated: 1m, 5m, 15m, running, total, last_pid
    Tsv,
    /// A single JSON object with typed fields
    Json,
}

/// The parsed `/proc/loadavg` line. Fields are kept as raw string tokens so the
/// TSV output is byte-faithful; JSON re-types them as numbers.
#[derive(Debug, PartialEq)]
struct LoadAvg {
    one: String,
    five: String,
    fifteen: String,
    running: String,
    total: String,
    last_pid: String,
}

pub fn run(args: &LoadavgArgs) -> Result<Outcome> {
    let raw = read_proc_file("/proc/loadavg")?;
    let Some(la) = parse_loadavg(&raw) else {
        return Ok(Outcome::NotFound);
    };

    let line = match args.format {
        Format::Tsv => format!(
            "{}\t{}\t{}\t{}\t{}\t{}",
            la.one, la.five, la.fifteen, la.running, la.total, la.last_pid
        ),
        Format::Json => serde_json::to_string(&build_json(&la))?,
    };

    let stdout = io::stdout();
    let handle = stdout.lock();
    let mut writer = BoundedWriter::new(handle, None);
    writer.write_line(&line)?;
    writer.flush()?;
    Ok(Outcome::Found)
}

/// Parse the first (and only) line of `/proc/loadavg`. Returns `None` if the
/// line has fewer than the five expected whitespace fields or the
/// `running/total` field lacks its `/`.
fn parse_loadavg(input: &str) -> Option<LoadAvg> {
    let line = input.lines().next()?;
    let t: Vec<&str> = line.split_whitespace().collect();
    if t.len() < 5 {
        return None;
    }
    let (running, total) = t[3].split_once('/')?;
    Some(LoadAvg {
        one: t[0].to_string(),
        five: t[1].to_string(),
        fifteen: t[2].to_string(),
        running: running.to_string(),
        total: total.to_string(),
        last_pid: t[4].to_string(),
    })
}

fn build_json(la: &LoadAvg) -> Value {
    json!({
        "1m": json_num(&la.one),
        "5m": json_num(&la.five),
        "15m": json_num(&la.fifteen),
        "running": json_num(&la.running),
        "total": json_num(&la.total),
        "last_pid": json_num(&la.last_pid),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_standard_line() {
        let la = parse_loadavg("3.14 2.71 1.41 1/284 12345\n").unwrap();
        assert_eq!(
            la,
            LoadAvg {
                one: "3.14".into(),
                five: "2.71".into(),
                fifteen: "1.41".into(),
                running: "1".into(),
                total: "284".into(),
                last_pid: "12345".into(),
            }
        );
    }

    #[test]
    fn rejects_short_line() {
        assert_eq!(parse_loadavg("3.14 2.71 1.41\n"), None);
    }

    #[test]
    fn rejects_running_field_without_slash() {
        assert_eq!(parse_loadavg("3.14 2.71 1.41 284 12345\n"), None);
    }

    #[test]
    fn json_is_typed() {
        let la = parse_loadavg("0.50 0.25 0.10 2/284 999\n").unwrap();
        let v = build_json(&la);
        assert_eq!(v["1m"], json!(0.5));
        assert_eq!(v["running"], json!(2));
        assert_eq!(v["total"], json!(284));
        assert_eq!(v["last_pid"], json!(999));
    }
}
