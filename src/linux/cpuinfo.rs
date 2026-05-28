//! `sak linux cpuinfo` — parse `/proc/cpuinfo` into one row per logical CPU.
//!
//! The kernel's `/proc/cpuinfo` layout varies by architecture: x86_64 carries
//! `vendor_id`, `model name`, `cpu MHz`, `cache size`, `cpu cores`, `siblings`
//! and a `flags` line, while aarch64 has a much sparser block (`processor`,
//! `BogoMIPS`, a `Features` line for the flag set, and `CPU implementer` /
//! `CPU part` identity fields) with no `model name` or `cache size` at all.
//!
//! Rather than make callers branch on which arch produced the file, this command
//! projects both into a fixed column set —
//! `cpu<TAB>vendor<TAB>model_name<TAB>mhz<TAB>cache_kb<TAB>cores<TAB>siblings<TAB>flags_count`
//! — rendering any field the arch does not provide as `-`. The flag set is
//! summarized as a count by default (the raw list is long); `--full` swaps the
//! last column for the space-joined flags. The parser ([`parse_cpuinfo`]) is a
//! pure function over `&str` so it is unit-tested on hand-built fixtures from
//! both architectures with no machine of that arch in the loop.

use crate::output::Outcome;
use std::collections::BTreeMap;
use std::io;

use anyhow::Result;
use clap::Args;
use serde_json::{Value, json};

use super::{parse_kv_line, read_proc_file, sanitize};
use crate::output::BoundedWriter;

#[derive(Args)]
#[command(
    about = "Parse /proc/cpuinfo into one row per logical CPU",
    long_about = "Parse /proc/cpuinfo and emit one row per logical CPU.\n\n\
        Default output is TSV with a fixed column set that does not depend on \
        the host architecture:\n\n  \
        cpu<TAB>vendor<TAB>model_name<TAB>mhz<TAB>cache_kb<TAB>cores<TAB>siblings<TAB>flags_count\n\n\
        Any field the running architecture does not expose renders as `-`. \
        aarch64, for example, has no `vendor_id`, `model name`, `cpu MHz`, \
        `cache size`, `cpu cores`, or `siblings`, so those columns are `-` and \
        `flags` is sourced from the aarch64 `Features` line.\n\n\
        The `cache_kb` column is the integer kilobyte count parsed out of the \
        `cache size: 8192 KB` line. The flag set is summarized as a count by \
        default because the raw list runs to dozens of entries; pass `--full` \
        to put the space-joined flag list in the last column instead.\n\n\
        `--format json` emits one JSON object per CPU (NDJSON); the `flags` \
        array is included only with `--full`, otherwise just `flags_count`.",
    after_help = "\
Examples:
  sak linux cpuinfo                  One row per logical CPU (flag counts)
  sak linux cpuinfo --full           Include the raw flag list per CPU
  sak linux cpuinfo --format json    NDJSON for further processing
  sak linux cpuinfo --limit 4        Cap output at 4 CPUs"
)]
pub struct CpuinfoArgs {
    /// Include the raw flag list instead of just its count
    #[arg(long)]
    pub full: bool,

    /// Output format
    #[arg(long, value_enum, default_value_t = Format::Tsv)]
    pub format: Format,

    /// Maximum number of output lines
    #[arg(long)]
    pub limit: Option<usize>,
}

#[derive(Clone, Copy, clap::ValueEnum)]
pub enum Format {
    /// Tab-separated columns
    Tsv,
    /// Newline-delimited JSON, one CPU per line
    Json,
}

/// One logical CPU's projected fields. Missing fields are stored as `-`
/// (except `flags`, where "missing" is simply an empty vector).
#[derive(Debug, PartialEq)]
struct CpuRecord {
    cpu: String,
    vendor: String,
    model_name: String,
    mhz: String,
    cache_kb: String,
    cores: String,
    siblings: String,
    flags: Vec<String>,
}

pub fn run(args: &CpuinfoArgs) -> Result<Outcome> {
    let raw = read_proc_file("/proc/cpuinfo")?;
    let records = parse_cpuinfo(&raw);

    let stdout = io::stdout();
    let handle = stdout.lock();
    let mut writer = BoundedWriter::new(handle, args.limit);

    let mut wrote_any = false;
    for rec in &records {
        let line = match args.format {
            Format::Tsv => format_row(rec, args.full),
            Format::Json => serde_json::to_string(&build_json(rec, args.full))?,
        };
        if !writer.write_line(&line)? {
            break;
        }
        wrote_any = true;
    }

    writer.flush()?;
    if wrote_any {
        Ok(Outcome::Found)
    } else {
        Ok(Outcome::NotFound)
    }
}

/// Parse the whole `/proc/cpuinfo` body into per-CPU records.
///
/// Blocks are separated by blank lines; within a block each line is a
/// `Key: value` pair. The `processor` field gives the logical CPU index; when a
/// block lacks it (some sparse layouts), the block's ordinal position is used as
/// a fallback so every CPU still gets a stable `cpu` value.
fn parse_cpuinfo(input: &str) -> Vec<CpuRecord> {
    let mut records = Vec::new();
    let mut current: BTreeMap<String, String> = BTreeMap::new();
    let mut ordinal = 0usize;

    for line in input.lines() {
        if line.trim().is_empty() {
            if !current.is_empty() {
                records.push(record_from(&current, ordinal));
                ordinal += 1;
                current.clear();
            }
            continue;
        }
        if let Some((key, value)) = parse_kv_line(line) {
            current.insert(key, value);
        }
    }
    if !current.is_empty() {
        records.push(record_from(&current, ordinal));
    }
    records
}

/// Build one [`CpuRecord`] from a parsed block, applying the field mapping that
/// papers over the x86_64 / aarch64 differences.
fn record_from(map: &BTreeMap<String, String>, ordinal: usize) -> CpuRecord {
    let field = |key: &str| -> String {
        map.get(key)
            .filter(|s| !s.is_empty())
            .map(|s| sanitize(s))
            .unwrap_or_else(|| "-".to_string())
    };

    let cpu = map
        .get("processor")
        .filter(|s| !s.is_empty())
        .cloned()
        .unwrap_or_else(|| ordinal.to_string());

    // `cache size` is reported as e.g. "8192 KB" — keep just the integer.
    let cache_kb = map
        .get("cache size")
        .and_then(|s| s.split_whitespace().next())
        .map(|s| s.to_string())
        .unwrap_or_else(|| "-".to_string());

    // x86_64 lists CPU features under `flags`; aarch64 under `Features`.
    let flags = map
        .get("flags")
        .or_else(|| map.get("Features"))
        .map(|s| s.split_whitespace().map(String::from).collect())
        .unwrap_or_default();

    CpuRecord {
        cpu,
        vendor: field("vendor_id"),
        model_name: field("model name"),
        mhz: field("cpu MHz"),
        cache_kb,
        cores: field("cpu cores"),
        siblings: field("siblings"),
        flags,
    }
}

fn format_row(rec: &CpuRecord, full: bool) -> String {
    let last = if full {
        if rec.flags.is_empty() {
            "-".to_string()
        } else {
            rec.flags.join(" ")
        }
    } else {
        rec.flags.len().to_string()
    };
    format!(
        "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
        rec.cpu, rec.vendor, rec.model_name, rec.mhz, rec.cache_kb, rec.cores, rec.siblings, last,
    )
}

fn build_json(rec: &CpuRecord, full: bool) -> Value {
    let mut obj = json!({
        "cpu": rec.cpu,
        "vendor": rec.vendor,
        "model_name": rec.model_name,
        "mhz": rec.mhz,
        "cache_kb": rec.cache_kb,
        "cores": rec.cores,
        "siblings": rec.siblings,
        "flags_count": rec.flags.len(),
    });
    if full {
        obj["flags"] = json!(rec.flags);
    }
    obj
}

#[cfg(test)]
mod tests {
    use super::*;

    const X86_CPUINFO: &str = "\
processor\t: 0
vendor_id\t: GenuineIntel
cpu family\t: 6
model\t\t: 142
model name\t: Intel(R) Core(TM) i7-8650U CPU @ 1.90GHz
stepping\t: 10
cpu MHz\t\t: 800.000
cache size\t: 8192 KB
physical id\t: 0
siblings\t: 8
core id\t\t: 0
cpu cores\t: 4
flags\t\t: fpu vme de pse tsc msr pae

processor\t: 1
vendor_id\t: GenuineIntel
cpu family\t: 6
model\t\t: 142
model name\t: Intel(R) Core(TM) i7-8650U CPU @ 1.90GHz
stepping\t: 10
cpu MHz\t\t: 900.000
cache size\t: 8192 KB
physical id\t: 0
siblings\t: 8
core id\t\t: 1
cpu cores\t: 4
flags\t\t: fpu vme de pse tsc msr pae sse sse2
";

    const AARCH64_CPUINFO: &str = "\
processor\t: 0
BogoMIPS\t: 50.00
Features\t: fp asimd evtstrm aes pmull sha1 sha2 crc32
CPU implementer\t: 0x41
CPU architecture: 8
CPU variant\t: 0x0
CPU part\t: 0xd08
CPU revision\t: 3

processor\t: 1
BogoMIPS\t: 50.00
Features\t: fp asimd evtstrm aes pmull sha1 sha2 crc32 cpuid
CPU implementer\t: 0x41
CPU architecture: 8
CPU variant\t: 0x0
CPU part\t: 0xd08
CPU revision\t: 3
";

    #[test]
    fn parses_x86_block_with_full_field_set() {
        let recs = parse_cpuinfo(X86_CPUINFO);
        assert_eq!(recs.len(), 2);
        let c0 = &recs[0];
        assert_eq!(c0.cpu, "0");
        assert_eq!(c0.vendor, "GenuineIntel");
        assert_eq!(c0.model_name, "Intel(R) Core(TM) i7-8650U CPU @ 1.90GHz");
        assert_eq!(c0.mhz, "800.000");
        assert_eq!(c0.cache_kb, "8192");
        assert_eq!(c0.cores, "4");
        assert_eq!(c0.siblings, "8");
        assert_eq!(c0.flags.len(), 7);
        assert_eq!(recs[1].cpu, "1");
        assert_eq!(recs[1].flags.len(), 9);
    }

    #[test]
    fn parses_aarch64_block_with_missing_fields_as_dash() {
        let recs = parse_cpuinfo(AARCH64_CPUINFO);
        assert_eq!(recs.len(), 2);
        let c0 = &recs[0];
        assert_eq!(c0.cpu, "0");
        assert_eq!(c0.vendor, "-");
        assert_eq!(c0.model_name, "-");
        assert_eq!(c0.mhz, "-");
        assert_eq!(c0.cache_kb, "-");
        assert_eq!(c0.cores, "-");
        assert_eq!(c0.siblings, "-");
        // Flags come from the aarch64 `Features` line.
        assert_eq!(c0.flags.len(), 8);
        assert_eq!(c0.flags[0], "fp");
    }

    #[test]
    fn falls_back_to_ordinal_when_processor_field_missing() {
        let input = "vendor_id\t: GenuineIntel\nflags\t: fpu\n";
        let recs = parse_cpuinfo(input);
        assert_eq!(recs.len(), 1);
        assert_eq!(recs[0].cpu, "0");
    }

    #[test]
    fn tsv_default_row_uses_flag_count() {
        let recs = parse_cpuinfo(X86_CPUINFO);
        assert_eq!(
            format_row(&recs[0], false),
            "0\tGenuineIntel\tIntel(R) Core(TM) i7-8650U CPU @ 1.90GHz\t800.000\t8192\t4\t8\t7"
        );
    }

    #[test]
    fn tsv_full_row_uses_flag_list() {
        let recs = parse_cpuinfo(X86_CPUINFO);
        let row = format_row(&recs[0], true);
        assert!(row.ends_with("\tfpu vme de pse tsc msr pae"));
    }

    #[test]
    fn full_row_renders_dash_for_empty_flags() {
        let rec = CpuRecord {
            cpu: "0".into(),
            vendor: "-".into(),
            model_name: "-".into(),
            mhz: "-".into(),
            cache_kb: "-".into(),
            cores: "-".into(),
            siblings: "-".into(),
            flags: vec![],
        };
        assert!(format_row(&rec, true).ends_with("\t-"));
    }

    #[test]
    fn json_omits_flags_array_unless_full() {
        let recs = parse_cpuinfo(AARCH64_CPUINFO);
        let compact = build_json(&recs[0], false);
        assert!(compact.get("flags").is_none());
        assert_eq!(compact["flags_count"], 8);

        let full = build_json(&recs[0], true);
        assert_eq!(full["flags"].as_array().unwrap().len(), 8);
    }
}
