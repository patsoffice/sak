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
//! The domain is **Linux-only** and gated by `#[cfg(target_os = "linux")]`:
//! the per-command parsers, the [`LinuxCommand`] enum, [`run`], and the
//! `/proc` helpers (`read_proc_file`, `parse_kv_line`, `sanitize`, `json_num`)
//! all drop out on non-Linux targets, taking their would-be dead-code warnings
//! with them. On macOS and friends the clap surface for `sak linux` therefore
//! disappears too — `sak linux --help` returns an "unrecognized subcommand"
//! error rather than advertising commands that can't run. The [`hook`]
//! submodule stays compiled on every target so the registry's `sysctl`
//! redirect message remains live even where the destination `sak linux sysctl`
//! command isn't.

pub mod hook;

#[cfg(target_os = "linux")]
pub mod cpuinfo;
#[cfg(target_os = "linux")]
pub mod loadavg;
#[cfg(target_os = "linux")]
pub mod meminfo;
#[cfg(target_os = "linux")]
pub mod mounts;
#[cfg(target_os = "linux")]
pub mod network;
#[cfg(target_os = "linux")]
pub mod process;
#[cfg(target_os = "linux")]
pub mod sysctl;
#[cfg(target_os = "linux")]
pub mod uptime;

#[cfg(target_os = "linux")]
use crate::output::Outcome;

#[cfg(target_os = "linux")]
use anyhow::Result;
#[cfg(target_os = "linux")]
use clap::Subcommand;

#[cfg(target_os = "linux")]
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

#[cfg(target_os = "linux")]
pub fn run(cmd: &LinuxCommand) -> Result<Outcome> {
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
#[cfg(target_os = "linux")]
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
#[cfg(target_os = "linux")]
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
#[cfg(target_os = "linux")]
pub fn sanitize(value: &str) -> String {
    value
        .chars()
        .map(|c| match c {
            '\n' | '\r' | '\t' => ' ',
            other => other,
        })
        .collect()
}

/// Re-type a numeric string as a JSON number — integer where it fits in a
/// `u64`, then float — falling back to the raw string if it parses as neither.
///
/// Shared by the JSON emitters across the `/proc` parsers so a numeric field
/// round-trips as a JSON number instead of a quoted string, while a non-numeric
/// value (or a `-` placeholder) stays a string. The `f64` branch is dormant for
/// integer-only fields like `network`'s `uid`/`inode` (the `u64` parse wins
/// first), but harmless there and required by `loadavg`/`uptime`, whose values
/// are genuinely fractional.
#[cfg(target_os = "linux")]
pub fn json_num(s: &str) -> serde_json::Value {
    use serde_json::json;
    if let Ok(n) = s.parse::<u64>() {
        json!(n)
    } else if let Ok(f) = s.parse::<f64>() {
        json!(f)
    } else {
        json!(s)
    }
}

#[cfg(all(test, target_os = "linux"))]
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

    #[test]
    fn json_num_prefers_integer_then_float_then_string() {
        use serde_json::json;
        assert_eq!(json_num("12345"), json!(12345u64));
        assert_eq!(json_num("2.71"), json!(2.71));
        assert_eq!(json_num("1/284"), json!("1/284"));
        assert_eq!(json_num("-"), json!("-"));
    }
}
