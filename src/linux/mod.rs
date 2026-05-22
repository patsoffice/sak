//! `linux` domain — read-only inspection of parsed `/proc` system state.
//!
//! `sak fs read /proc/...` already covers the byte-level case, so this domain
//! is **not** a safety guardrail — it is a *parsing convenience*. The value-add
//! is sparing LLMs from fragile `awk` against kernel-version-varying layouts:
//! every command turns a well-known `/proc` file into stable TSV / JSON with a
//! fixed column set, rendering missing fields as `-` so a consumer never has to
//! branch on which arch or kernel produced the file.
//!
//! Scope is a *curated set of parsers*, not a generic `/proc` walker. There is
//! no new dependency, no cargo feature, and no chokepoint module: the kernel
//! rejects writes to these interfaces by file permission, so there is no
//! mutation surface to guard. Every parser is a pure function on `&str`,
//! unit-tested on hand-built fixtures (mirroring the `k8s::containers` pattern).
//!
//! The domain is **Linux-only**. The clap surface (subcommands, `--help`) is
//! compiled on every platform so `sak linux --help` is discoverable anywhere,
//! but [`run`] short-circuits on non-Linux targets with a clear error and exit
//! code 2 rather than reading a `/proc` that does not exist.

pub mod cpuinfo;
pub mod loadavg;
pub mod meminfo;
pub mod mounts;
pub mod network;
pub mod process;
pub mod sysctl;
pub mod uptime;

use std::process::ExitCode;

use anyhow::Result;
use clap::Subcommand;

#[derive(Subcommand)]
pub enum LinuxCommand {
    Cpuinfo(cpuinfo::CpuinfoArgs),
    Meminfo(meminfo::MeminfoArgs),
    Mounts(mounts::MountsArgs),
    Loadavg(loadavg::LoadavgArgs),
    Uptime(uptime::UptimeArgs),
    Sysctl(sysctl::SysctlArgs),
    Process(process::ProcessArgs),
    Network(network::NetworkArgs),
}

pub fn run(cmd: &LinuxCommand) -> Result<ExitCode> {
    // The clap types compile everywhere for a consistent `--help`, but the
    // commands themselves only make sense where `/proc` exists. Reject other
    // platforms here so the failure is a clear message, not a confusing
    // "No such file or directory" from deep inside a parser.
    #[cfg(not(target_os = "linux"))]
    {
        let _ = cmd;
        anyhow::bail!("the `linux` domain is only available on Linux targets");
    }
    #[cfg(target_os = "linux")]
    match cmd {
        LinuxCommand::Cpuinfo(args) => cpuinfo::run(args),
        LinuxCommand::Meminfo(args) => meminfo::run(args),
        LinuxCommand::Mounts(args) => mounts::run(args),
        LinuxCommand::Loadavg(args) => loadavg::run(args),
        LinuxCommand::Uptime(args) => uptime::run(args),
        LinuxCommand::Sysctl(args) => sysctl::run(args),
        LinuxCommand::Process(args) => process::run(args),
        LinuxCommand::Network(args) => network::run(args),
    }
}

/// Read a file from `/proc`, enforcing the `/proc/` path prefix.
///
/// The prefix check keeps the domain honest: every command reads a curated
/// kernel-interface path, never an arbitrary file the caller smuggled in.
/// `/sys` device-tree parsers are an explicit follow-up; until they land this
/// helper deliberately rejects anything outside `/proc/`.
pub fn read_proc_file(path: &str) -> Result<String> {
    use anyhow::{Context, bail};
    if !path.starts_with("/proc/") {
        bail!("refusing to read {path}: the linux domain only reads paths under /proc/");
    }
    std::fs::read_to_string(path).with_context(|| format!("read {path}"))
}

/// Parse a `Key: value` line of the shape `/proc` uses pervasively
/// (`/proc/meminfo`, `/proc/cpuinfo`, `/proc/<pid>/status`, ...).
///
/// Splits on the first `:` only — values that themselves contain a colon (e.g.
/// cpuinfo's `address sizes : 39 bits physical, 48 bits virtual`) keep the
/// remainder intact. Both key and value are trimmed of the surrounding tabs and
/// spaces these files pad with. Returns `None` for lines with no colon or an
/// empty key (blank separators, section headers).
pub fn parse_kv_line(line: &str) -> Option<(String, String)> {
    let (key, value) = line.split_once(':')?;
    let key = key.trim();
    if key.is_empty() {
        return None;
    }
    Some((key.to_string(), value.trim().to_string()))
}

/// Replace newline, carriage return, and tab characters with a single space so
/// a value never breaks a `key<TAB>value` or column-oriented TSV line. Mirrors
/// the defense `sak sqlite info` uses; kept local because the sqlite copy lives
/// behind the optional `sqlite` cargo feature and the linux domain is always on.
pub fn sanitize(value: &str) -> String {
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

    #[test]
    fn read_proc_file_rejects_paths_outside_proc() {
        let err = read_proc_file("/etc/passwd").unwrap_err();
        assert!(err.to_string().contains("only reads paths under /proc/"));
    }

    #[test]
    fn parse_kv_line_splits_on_first_colon() {
        assert_eq!(
            parse_kv_line("model name\t: Intel(R) Core(TM) i7"),
            Some(("model name".to_string(), "Intel(R) Core(TM) i7".to_string()))
        );
    }

    #[test]
    fn parse_kv_line_keeps_value_colons() {
        assert_eq!(
            parse_kv_line("address sizes\t: 39 bits physical, 48 bits virtual"),
            Some((
                "address sizes".to_string(),
                "39 bits physical, 48 bits virtual".to_string()
            ))
        );
    }

    #[test]
    fn parse_kv_line_rejects_lines_without_colon() {
        assert_eq!(parse_kv_line(""), None);
        assert_eq!(parse_kv_line("just some text"), None);
    }

    #[test]
    fn sanitize_collapses_control_whitespace() {
        assert_eq!(sanitize("a\tb\nc\rd"), "a b c d");
        assert_eq!(sanitize("no control chars"), "no control chars");
    }
}
