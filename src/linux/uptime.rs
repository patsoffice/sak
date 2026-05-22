//! `sak linux uptime` — parse the single line of `/proc/uptime`.
//!
//! `/proc/uptime` is two floats: seconds since boot, and the summed idle time
//! across all CPUs (so on a multi-core box idle can exceed wall-clock uptime).
//! Default output is the two raw values as TSV; `--human` re-renders the uptime
//! column as `Xd Yh Zm`. The arithmetic lives in [`humanize`], a pure function
//! unit-tested without touching `/proc`.

use std::io;
use std::process::ExitCode;

use anyhow::Result;
use clap::Args;
use serde_json::{Value, json};

use super::read_proc_file;
use crate::output::BoundedWriter;

#[derive(Args)]
#[command(
    about = "Parse /proc/uptime into uptime + idle seconds",
    long_about = "Parse the single line of /proc/uptime.\n\n\
        Default output is one TSV row:\n\n  \
        uptime_seconds<TAB>idle_seconds\n\n\
        `uptime_seconds` is wall-clock time since boot; `idle_seconds` is the \
        sum of every CPU's idle time, so on a multi-core machine it is normally \
        larger than the uptime.\n\n\
        `--human` re-renders the uptime column as `Xd Yh Zm` (the idle column \
        is left as raw seconds). `--format json` emits a single JSON object with \
        the two values as numbers, plus an `uptime_human` string when `--human` \
        is set.",
    after_help = "\
Examples:
  sak linux uptime                   TSV: uptime_seconds idle_seconds
  sak linux uptime --human           Render uptime as `Xd Yh Zm`
  sak linux uptime --format json     Typed JSON object"
)]
pub struct UptimeArgs {
    /// Render uptime_seconds as `Xd Yh Zm`
    #[arg(long)]
    pub human: bool,

    /// Output format
    #[arg(long, value_enum, default_value_t = Format::Tsv)]
    pub format: Format,
}

#[derive(Clone, Copy, clap::ValueEnum)]
pub enum Format {
    /// Tab-separated: uptime_seconds, idle_seconds
    Tsv,
    /// A single JSON object with typed fields
    Json,
}

/// The parsed `/proc/uptime` line. Raw string tokens keep the TSV output
/// byte-faithful; JSON re-types them as numbers.
#[derive(Debug, PartialEq)]
struct Uptime {
    uptime: String,
    idle: String,
}

pub fn run(args: &UptimeArgs) -> Result<ExitCode> {
    let raw = read_proc_file("/proc/uptime")?;
    let Some(up) = parse_uptime(&raw) else {
        return Ok(ExitCode::from(1));
    };

    let line = match args.format {
        Format::Tsv => {
            let first = if args.human {
                up.uptime
                    .parse::<f64>()
                    .map(humanize)
                    .unwrap_or_else(|_| up.uptime.clone())
            } else {
                up.uptime.clone()
            };
            format!("{}\t{}", first, up.idle)
        }
        Format::Json => serde_json::to_string(&build_json(&up, args.human))?,
    };

    let stdout = io::stdout();
    let handle = stdout.lock();
    let mut writer = BoundedWriter::new(handle, None);
    writer.write_line(&line)?;
    writer.flush()?;
    Ok(ExitCode::SUCCESS)
}

/// Parse the first (and only) line of `/proc/uptime`. Returns `None` if the line
/// has fewer than the two expected whitespace fields.
fn parse_uptime(input: &str) -> Option<Uptime> {
    let line = input.lines().next()?;
    let t: Vec<&str> = line.split_whitespace().collect();
    if t.len() < 2 {
        return None;
    }
    Some(Uptime {
        uptime: t[0].to_string(),
        idle: t[1].to_string(),
    })
}

/// Render a duration in seconds as `Xd Yh Zm`, truncating sub-minute seconds.
/// All three units are always shown for predictable, parseable output.
fn humanize(seconds: f64) -> String {
    let total = seconds.max(0.0) as u64;
    let days = total / 86_400;
    let hours = (total % 86_400) / 3_600;
    let mins = (total % 3_600) / 60;
    format!("{days}d {hours}h {mins}m")
}

/// Re-type a numeric string as a JSON number (integer where possible, then
/// float), falling back to the raw string if it does not parse.
fn json_num(s: &str) -> Value {
    if let Ok(n) = s.parse::<u64>() {
        json!(n)
    } else if let Ok(f) = s.parse::<f64>() {
        json!(f)
    } else {
        json!(s)
    }
}

fn build_json(up: &Uptime, human: bool) -> Value {
    let mut obj = json!({
        "uptime_seconds": json_num(&up.uptime),
        "idle_seconds": json_num(&up.idle),
    });
    if human && let Ok(secs) = up.uptime.parse::<f64>() {
        obj["uptime_human"] = json!(humanize(secs));
    }
    obj
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_standard_line() {
        let up = parse_uptime("12345.67 98765.43\n").unwrap();
        assert_eq!(
            up,
            Uptime {
                uptime: "12345.67".into(),
                idle: "98765.43".into(),
            }
        );
    }

    #[test]
    fn rejects_short_line() {
        assert_eq!(parse_uptime("12345.67\n"), None);
    }

    #[test]
    fn humanize_breaks_down_days_hours_minutes() {
        // 1 day, 1 hour, 1 minute, 1 second -> seconds truncated.
        assert_eq!(humanize(90_061.0), "1d 1h 1m");
        assert_eq!(humanize(0.0), "0d 0h 0m");
        assert_eq!(humanize(3_661.0), "0d 1h 1m");
        assert_eq!(humanize(172_800.0), "2d 0h 0m");
    }

    #[test]
    fn humanize_clamps_negative_to_zero() {
        assert_eq!(humanize(-5.0), "0d 0h 0m");
    }

    #[test]
    fn json_adds_human_only_when_requested() {
        let up = parse_uptime("90061.0 100000.0\n").unwrap();
        let plain = build_json(&up, false);
        assert!(plain.get("uptime_human").is_none());
        assert_eq!(plain["uptime_seconds"], json!(90061.0));

        let human = build_json(&up, true);
        assert_eq!(human["uptime_human"], json!("1d 1h 1m"));
    }
}
