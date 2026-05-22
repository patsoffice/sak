//! `sak linux process` — parse `/proc/<pid>/status`, `/proc/<pid>/stat`, and
//! `/proc/<pid>/cmdline` into one record per process.
//!
//! `/proc/<pid>/status` is clean `Key: value` and supplies most fields; the
//! process name and run state are pulled from `/proc/<pid>/stat`, whose second
//! field (`comm`) is parenthesized and *may contain spaces and `)`* — e.g.
//! `1234 ((sd-pam)) S ...`. The parser finds the matching `)` from the end of
//! the line backward so a parenthesized name never throws off the fields after
//! it. `cmdline` is the NUL-separated argv joined with spaces (kernel threads
//! have an empty cmdline and render as `[name]`), collapsed to one line and
//! truncated with `…` so a row stays scannable.
//!
//! Operate on a single PID (positional) or pass `--all` to sweep every numeric
//! `/proc` subdir. The status/stat/cmdline parsers are pure functions, unit
//! tested on hand-built fixtures including the spaces-and-parens `comm` case.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::process::ExitCode;

use anyhow::{Result, bail};
use clap::Args;
use serde_json::{Map, Value, json};

use super::{parse_kv_line, sanitize};
use crate::output::BoundedWriter;

/// cmdline / name truncation budget (characters, not bytes).
const CMDLINE_MAX: usize = 200;

#[derive(Args)]
#[command(
    about = "Parse /proc/<pid> status, stat, and cmdline into process records",
    long_about = "Parse a process's /proc/<pid>/status, /proc/<pid>/stat, and \
        /proc/<pid>/cmdline into one record.\n\n\
        Default TSV column set:\n\n  \
        pid<TAB>name<TAB>state<TAB>ppid<TAB>uid<TAB>gid<TAB>vm_rss_kb<TAB>vm_size_kb<TAB>threads<TAB>cmdline\n\n\
        `name` and `state` come from /proc/<pid>/stat (whose parenthesized \
        `comm` field may contain spaces); the rest come from \
        /proc/<pid>/status. `uid`/`gid` are the real IDs. Memory fields are \
        absent for kernel threads and render as `-`. `cmdline` is the argv \
        joined with spaces (an empty cmdline — a kernel thread — renders as \
        `[name]`), collapsed to one line and truncated to keep the row \
        scannable.\n\n\
        Pass a single PID, or `--all` to emit a row for every process. \
        `--format json` emits the full status parse plus the stat-derived name, \
        state, and cmdline (one object per line under `--all`).",
    after_help = "\
Examples:
  sak linux process 1                One process by PID
  sak linux process --all            A row per process (table view)
  sak linux process --all --limit 20 Cap the table at 20 rows
  sak linux process 1 --format json  Full status/stat parse as JSON"
)]
pub struct ProcessArgs {
    /// PID to inspect (omit and pass --all for every process)
    pub pid: Option<u32>,

    /// Emit a record for every process (every numeric /proc subdir)
    #[arg(long)]
    pub all: bool,

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
    /// Newline-delimited JSON, one process per line (full status parse)
    Json,
}

/// One process's projected fields. Missing fields are `-`. `status` keeps the
/// full /proc/<pid>/status parse for the JSON output.
#[derive(Debug, PartialEq)]
struct Process {
    pid: String,
    name: String,
    state: String,
    ppid: String,
    uid: String,
    gid: String,
    vm_rss_kb: String,
    vm_size_kb: String,
    threads: String,
    cmdline: String,
    status: BTreeMap<String, String>,
}

pub fn run(args: &ProcessArgs) -> Result<ExitCode> {
    if args.all && args.pid.is_some() {
        bail!("pass either a PID or --all, not both");
    }
    if !args.all && args.pid.is_none() {
        bail!("specify a PID or pass --all");
    }

    let mut procs: Vec<Process> = Vec::new();
    if args.all {
        let mut pids: Vec<u32> = Vec::new();
        for entry in fs::read_dir("/proc")? {
            let Ok(entry) = entry else { continue };
            if let Some(name) = entry.file_name().to_str()
                && let Ok(pid) = name.parse::<u32>()
            {
                pids.push(pid);
            }
        }
        pids.sort_unstable();
        for pid in pids {
            // A process that exits mid-sweep simply drops out.
            if let Some(p) = collect_process(&pid.to_string()) {
                procs.push(p);
            }
        }
    } else if let Some(pid) = args.pid {
        match collect_process(&pid.to_string()) {
            Some(p) => procs.push(p),
            None => return Ok(ExitCode::from(1)),
        }
    }

    let stdout = io::stdout();
    let handle = stdout.lock();
    let mut writer = BoundedWriter::new(handle, args.limit);

    let mut wrote_any = false;
    for p in &procs {
        let line = match args.format {
            Format::Tsv => format_row(p),
            Format::Json => serde_json::to_string(&build_json(p))?,
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

/// Read and assemble a single process record, or `None` if the process is gone
/// or its status/stat can't be read.
fn collect_process(pid: &str) -> Option<Process> {
    let status_raw = fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
    let status = parse_status(&status_raw);
    let stat_raw = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let (comm, state) = parse_stat_comm_state(&stat_raw)?;
    let cmdline_raw = fs::read(format!("/proc/{pid}/cmdline")).unwrap_or_default();
    let cmdline = decode_cmdline(&cmdline_raw, &comm);
    Some(assemble(pid, status, &comm, &state, &cmdline))
}

/// Parse `/proc/<pid>/status` (clean `Key: value` lines) into a map.
fn parse_status(input: &str) -> BTreeMap<String, String> {
    let mut map = BTreeMap::new();
    for line in input.lines() {
        if let Some((k, v)) = parse_kv_line(line) {
            map.insert(k, v);
        }
    }
    map
}

/// Pull the process name (`comm`) and run state from a `/proc/<pid>/stat` line.
///
/// `stat` is whitespace-separated *except* field 2, `comm`, which is wrapped in
/// parentheses and may itself contain spaces and `)` (e.g. `((sd-pam))`). We
/// take everything between the first `(` and the *last* `)`, then read the run
/// state as the first token after that closing paren.
fn parse_stat_comm_state(input: &str) -> Option<(String, String)> {
    let open = input.find('(')?;
    let close = input.rfind(')')?;
    if close < open {
        return None;
    }
    let comm = input[open + 1..close].to_string();
    let state = input[close + 1..].split_whitespace().next()?.to_string();
    Some((comm, state))
}

/// Decode `/proc/<pid>/cmdline`: NUL-separated argv joined with spaces, one line,
/// truncated. An empty cmdline (kernel thread) renders as `[name]`.
fn decode_cmdline(raw: &[u8], name: &str) -> String {
    let text = String::from_utf8_lossy(raw);
    let joined = text
        .split('\0')
        .filter(|p| !p.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    let joined = sanitize(joined.trim());
    if joined.is_empty() {
        format!("[{name}]")
    } else {
        truncate_ellipsis(&joined, CMDLINE_MAX)
    }
}

/// Truncate to at most `max` characters, appending `…` when shortened.
fn truncate_ellipsis(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let head: String = s.chars().take(max).collect();
        format!("{head}…")
    }
}

/// Combine the parsed status map and stat-derived comm/state into a record.
fn assemble(
    pid: &str,
    status: BTreeMap<String, String>,
    comm: &str,
    state: &str,
    cmdline: &str,
) -> Process {
    // First whitespace token of a status value (Uid/Gid are tab-separated quads;
    // VmRSS/VmSize are "<n> kB"), or "-" when the field is absent.
    let first = |key: &str| -> String {
        status
            .get(key)
            .and_then(|v| v.split_whitespace().next())
            .map(str::to_string)
            .unwrap_or_else(|| "-".to_string())
    };

    Process {
        pid: pid.to_string(),
        name: sanitize(comm),
        state: state.to_string(),
        ppid: first("PPid"),
        uid: first("Uid"),
        gid: first("Gid"),
        vm_rss_kb: first("VmRSS"),
        vm_size_kb: first("VmSize"),
        threads: first("Threads"),
        cmdline: cmdline.to_string(),
        status,
    }
}

fn format_row(p: &Process) -> String {
    format!(
        "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
        p.pid,
        p.name,
        p.state,
        p.ppid,
        p.uid,
        p.gid,
        p.vm_rss_kb,
        p.vm_size_kb,
        p.threads,
        p.cmdline,
    )
}

fn build_json(p: &Process) -> Value {
    let status: Map<String, Value> = p
        .status
        .iter()
        .map(|(k, v)| (k.clone(), json!(v)))
        .collect();
    json!({
        "pid": p.pid.parse::<u64>().map(Value::from).unwrap_or_else(|_| json!(p.pid)),
        "name": p.name,
        "state": p.state,
        "ppid": p.ppid,
        "uid": p.uid,
        "gid": p.gid,
        "vm_rss_kb": p.vm_rss_kb,
        "vm_size_kb": p.vm_size_kb,
        "threads": p.threads,
        "cmdline": p.cmdline,
        "status": status,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const STATUS: &str = "\
Name:\tbash
Umask:\t0022
State:\tS (sleeping)
Tgid:\t1234
Pid:\t1234
PPid:\t1000
Uid:\t1000\t1000\t1000\t1000
Gid:\t1000\t1000\t1000\t1000
Threads:\t1
VmSize:\t   22000 kB
VmRSS:\t    5000 kB
";

    #[test]
    fn comm_with_spaces_and_inner_parens() {
        // The classic spaces-and-parens case: comm is "(sd-pam)".
        let (comm, state) = parse_stat_comm_state("1234 ((sd-pam)) S 1000 1234 1234 0 -1").unwrap();
        assert_eq!(comm, "(sd-pam)");
        assert_eq!(state, "S");
    }

    #[test]
    fn comm_with_spaces() {
        let (comm, state) = parse_stat_comm_state("42 (my proc) R 1 42 42 0").unwrap();
        assert_eq!(comm, "my proc");
        assert_eq!(state, "R");
    }

    #[test]
    fn stat_without_parens_is_rejected() {
        assert_eq!(parse_stat_comm_state("not a stat line"), None);
    }

    #[test]
    fn assemble_pulls_real_ids_and_memory() {
        let status = parse_status(STATUS);
        let p = assemble("1234", status, "bash", "S", "/usr/bin/bash -i");
        assert_eq!(p.pid, "1234");
        assert_eq!(p.name, "bash");
        assert_eq!(p.state, "S");
        assert_eq!(p.ppid, "1000");
        assert_eq!(p.uid, "1000");
        assert_eq!(p.gid, "1000");
        assert_eq!(p.vm_rss_kb, "5000");
        assert_eq!(p.vm_size_kb, "22000");
        assert_eq!(p.threads, "1");
        assert_eq!(p.cmdline, "/usr/bin/bash -i");
    }

    #[test]
    fn missing_memory_fields_render_as_dash() {
        // A kernel thread has no VmRSS/VmSize lines.
        let status = parse_status(
            "Name:\tkthreadd\nPPid:\t2\nUid:\t0\t0\t0\t0\nGid:\t0\t0\t0\t0\nThreads:\t1\n",
        );
        let p = assemble("2", status, "kthreadd", "S", "[kthreadd]");
        assert_eq!(p.vm_rss_kb, "-");
        assert_eq!(p.vm_size_kb, "-");
    }

    #[test]
    fn cmdline_joins_nul_separated_argv() {
        let raw = b"/usr/bin/foo\0--bar\0baz\0";
        assert_eq!(decode_cmdline(raw, "foo"), "/usr/bin/foo --bar baz");
    }

    #[test]
    fn empty_cmdline_renders_bracketed_name() {
        assert_eq!(decode_cmdline(b"", "kthreadd"), "[kthreadd]");
    }

    #[test]
    fn cmdline_is_truncated_with_ellipsis() {
        let long = format!("/bin/x {}", "a".repeat(500));
        let raw: Vec<u8> = long.bytes().collect();
        let out = decode_cmdline(&raw, "x");
        assert_eq!(out.chars().count(), CMDLINE_MAX + 1); // +1 for the ellipsis
        assert!(out.ends_with('…'));
    }

    #[test]
    fn json_includes_full_status_map() {
        let status = parse_status(STATUS);
        let p = assemble("1234", status, "bash", "S", "/usr/bin/bash");
        let v = build_json(&p);
        assert_eq!(v["pid"], json!(1234));
        assert_eq!(v["status"]["Umask"], "0022");
        assert_eq!(v["status"]["State"], "S (sleeping)");
    }
}
