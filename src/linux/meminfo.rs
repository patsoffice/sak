//! `sak linux meminfo` — parse `/proc/meminfo` into `key<TAB>value_kb` lines.
//!
//! `/proc/meminfo` is the simplest of the curated parsers: every line is
//! `Key: <number> kB`, except a handful of `HugePages_*` counters that are
//! unitless (`HugePages_Total: 0`). The command keeps just the numeric value in
//! both cases — the `value_kb` column is kilobytes for the sized fields and a
//! plain count for the huge-page counters, matching what the kernel reports.
//!
//! Output is sorted by key name by default for deterministic diffs; `--order
//! file` preserves the kernel's emission order instead. The parser
//! ([`parse_meminfo`]) is a pure function over `&str`, unit-tested on a
//! hand-built fixture covering both the `kB`-suffixed and unitless shapes.

use std::io;
use std::process::ExitCode;

use anyhow::Result;
use clap::Args;
use serde_json::{Value, json};

use super::parse_kv_line;
use super::read_proc_file;
use crate::output::BoundedWriter;

#[derive(Args)]
#[command(
    about = "Parse /proc/meminfo into key<TAB>value_kb lines",
    long_about = "Parse /proc/meminfo and emit one `key<TAB>value_kb` line per \
        field.\n\n\
        Every meminfo line is `Key: <number> kB`, except the `HugePages_*` \
        counters, which are unitless. This command keeps just the numeric value \
        in both cases: the `value_kb` column is kilobytes for the sized fields \
        and a plain count for the huge-page counters (and `Hugepagesize` is back \
        to kB).\n\n\
        Output is sorted by key name by default so two captures diff cleanly; \
        pass `--order file` to preserve the kernel's emission order instead.\n\n\
        `--format json` emits one JSON object per field (NDJSON) with a numeric \
        `value_kb` when the value parses as an integer.",
    after_help = "\
Examples:
  sak linux meminfo                  All fields, sorted by name
  sak linux meminfo --order file     Preserve kernel emission order
  sak linux meminfo --format json    NDJSON for further processing
  sak linux meminfo --limit 5        Cap output at 5 fields"
)]
pub struct MeminfoArgs {
    /// Row ordering: by key name (deterministic) or kernel file order
    #[arg(long, value_enum, default_value_t = Order::Name)]
    pub order: Order,

    /// Output format
    #[arg(long, value_enum, default_value_t = Format::Tsv)]
    pub format: Format,

    /// Maximum number of output lines
    #[arg(long)]
    pub limit: Option<usize>,
}

#[derive(Clone, Copy, clap::ValueEnum)]
pub enum Order {
    /// Sort by key name (deterministic)
    Name,
    /// Preserve the order the kernel emits fields in
    File,
}

#[derive(Clone, Copy, clap::ValueEnum)]
pub enum Format {
    /// Tab-separated: key<TAB>value_kb
    Tsv,
    /// Newline-delimited JSON, one field per line
    Json,
}

pub fn run(args: &MeminfoArgs) -> Result<ExitCode> {
    let raw = read_proc_file("/proc/meminfo")?;
    let mut fields = parse_meminfo(&raw);

    if let Order::Name = args.order {
        fields.sort_by(|a, b| a.0.cmp(&b.0));
    }

    let stdout = io::stdout();
    let handle = stdout.lock();
    let mut writer = BoundedWriter::new(handle, args.limit);

    let mut wrote_any = false;
    for (key, value) in &fields {
        let line = match args.format {
            Format::Tsv => format!("{key}\t{value}"),
            Format::Json => serde_json::to_string(&build_json(key, value))?,
        };
        if !writer.write_line(&line)? {
            break;
        }
        wrote_any = true;
    }

    writer.flush()?;
    if wrote_any {
        Ok(ExitCode::SUCCESS)
    } else {
        Ok(ExitCode::from(1))
    }
}

/// Parse `/proc/meminfo` into `(key, value)` pairs in file order.
///
/// Each line is `Key: <number>[ kB]`. Only the leading numeric token is kept,
/// dropping the `kB` unit suffix where present (and leaving unitless
/// `HugePages_*` counters untouched). Lines without a colon or a value are
/// skipped.
fn parse_meminfo(input: &str) -> Vec<(String, String)> {
    let mut fields = Vec::new();
    for line in input.lines() {
        let Some((key, value)) = parse_kv_line(line) else {
            continue;
        };
        // Keep the leading numeric token, dropping the trailing " kB" unit.
        let Some(num) = value.split_whitespace().next() else {
            continue;
        };
        fields.push((key, num.to_string()));
    }
    fields
}

fn build_json(key: &str, value: &str) -> Value {
    // Emit a number when the value parses as an integer; fall back to the raw
    // string otherwise so a surprise non-numeric field still round-trips.
    let value_kb = match value.parse::<u64>() {
        Ok(n) => json!(n),
        Err(_) => json!(value),
    };
    json!({ "key": key, "value_kb": value_kb })
}

#[cfg(test)]
mod tests {
    use super::*;

    const MEMINFO: &str = "\
MemTotal:       16384256 kB
MemFree:         1234567 kB
MemAvailable:    8000000 kB
Buffers:          123456 kB
Cached:          2345678 kB
SwapTotal:       2097148 kB
SwapFree:        2097148 kB
HugePages_Total:       0
HugePages_Free:        0
HugePages_Rsvd:        0
HugePages_Surp:        0
Hugepagesize:       2048 kB
DirectMap4k:      198388 kB
";

    #[test]
    fn parses_sized_and_unitless_fields() {
        let fields = parse_meminfo(MEMINFO);
        assert_eq!(fields.len(), 13);
        // First field stays in file order (no sorting in the parser).
        assert_eq!(fields[0], ("MemTotal".to_string(), "16384256".to_string()));
        // The kB suffix is dropped from a sized field.
        let cached = fields.iter().find(|(k, _)| k == "Cached").unwrap();
        assert_eq!(cached.1, "2345678");
        // Unitless huge-page counters keep their plain count.
        let hp = fields.iter().find(|(k, _)| k == "HugePages_Total").unwrap();
        assert_eq!(hp.1, "0");
        // Hugepagesize is back to a kB value.
        let hps = fields.iter().find(|(k, _)| k == "Hugepagesize").unwrap();
        assert_eq!(hps.1, "2048");
    }

    #[test]
    fn skips_lines_without_colon() {
        let fields = parse_meminfo("MemTotal: 100 kB\ngarbage line\nMemFree: 50 kB\n");
        assert_eq!(fields.len(), 2);
    }

    #[test]
    fn name_order_sorts_alphabetically() {
        let mut fields = parse_meminfo(MEMINFO);
        fields.sort_by(|a, b| a.0.cmp(&b.0));
        let keys: Vec<&str> = fields.iter().map(|(k, _)| k.as_str()).collect();
        let mut sorted = keys.clone();
        sorted.sort_unstable();
        assert_eq!(keys, sorted);
        assert_eq!(keys[0], "Buffers");
    }

    #[test]
    fn json_emits_numeric_value() {
        assert_eq!(
            build_json("MemTotal", "16384256"),
            json!({ "key": "MemTotal", "value_kb": 16384256 })
        );
    }

    #[test]
    fn json_falls_back_to_string_for_non_numeric() {
        assert_eq!(
            build_json("Weird", "n/a"),
            json!({ "key": "Weird", "value_kb": "n/a" })
        );
    }
}
