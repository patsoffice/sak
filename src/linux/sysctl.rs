//! `sak linux sysctl` — walk `/proc/sys` and emit `key<TAB>value` lines.
//!
//! This is the read side of `sysctl -a`: every regular file under `/proc/sys`
//! becomes one `key<TAB>value` row, with the path turned into the familiar
//! dotted form (`net/ipv4/tcp_syncookies` → `net.ipv4.tcp_syncookies`). An
//! optional positional regex filters the dotted key name. Values are run through
//! the domain's [`sanitize`](super::sanitize) helper so the tab-separated and
//! multi-line sysctls (e.g. `net.ipv4.tcp_rmem`) collapse to a single space and
//! never break the `key<TAB>value` contract.
//!
//! Reads that fail (write-only knobs like `net.ipv4.route.flush`, or
//! permission-denied entries) are silently skipped — `sysctl -a` does the same.
//! Output is sorted by key for deterministic diffs.

use std::io;
use std::path::Path;
use std::process::ExitCode;

use anyhow::{Context, Result};
use clap::Args;
use regex::Regex;
use serde_json::json;
use walkdir::WalkDir;

use super::sanitize;
use crate::output::BoundedWriter;

const SYSCTL_ROOT: &str = "/proc/sys";

#[derive(Args)]
#[command(
    about = "Walk /proc/sys and emit sysctl key<TAB>value lines",
    long_about = "Walk /proc/sys and emit one `key<TAB>value` line per knob, the \
        read side of `sysctl -a`.\n\n\
        Each path under /proc/sys is rendered in the dotted form `sysctl -a` \
        uses: `/proc/sys/net/ipv4/tcp_syncookies` becomes \
        `net.ipv4.tcp_syncookies`. Values are collapsed to a single line \
        (tab-separated sysctls like `net.ipv4.tcp_rmem` and any multi-line \
        values have their tabs/newlines replaced with spaces) so the \
        `key<TAB>value` contract always holds.\n\n\
        The optional positional PATTERN is a regex matched against the dotted \
        key name; with no pattern every readable knob is emitted. Write-only or \
        permission-denied knobs are silently skipped, just like `sysctl -a`. \
        Output is sorted by key.\n\n\
        `--format json` emits one `{\"key\":..., \"value\":...}` object per line \
        (NDJSON).",
    after_help = "\
Examples:
  sak linux sysctl                       Every readable sysctl, sorted by key
  sak linux sysctl '^net\\.ipv4\\.'        Only net.ipv4.* knobs
  sak linux sysctl tcp_syncookies        Any key matching the regex
  sak linux sysctl --format json         NDJSON for further processing
  sak linux sysctl --limit 50            Cap output at 50 knobs"
)]
pub struct SysctlArgs {
    /// Regex filter on the dotted key name (optional; default: all keys)
    pub pattern: Option<String>,

    /// Output format
    #[arg(long, value_enum, default_value_t = Format::Tsv)]
    pub format: Format,

    /// Maximum number of output lines
    #[arg(long)]
    pub limit: Option<usize>,
}

#[derive(Clone, Copy, clap::ValueEnum)]
pub enum Format {
    /// Tab-separated: key<TAB>value
    Tsv,
    /// Newline-delimited JSON, one knob per line
    Json,
}

pub fn run(args: &SysctlArgs) -> Result<ExitCode> {
    let re = match &args.pattern {
        Some(p) => Some(Regex::new(p).with_context(|| format!("invalid pattern regex: {p}"))?),
        None => None,
    };

    let base = Path::new(SYSCTL_ROOT);
    let mut rows: Vec<(String, String)> = Vec::new();

    let walker = WalkDir::new(base).into_iter().filter_entry(|e| {
        // Never descend through a symlinked directory — it could escape
        // /proc/sys or cycle. depth() > 0 keeps the walk root from being
        // filtered out by this check (matches the fs domain's convention).
        !(e.depth() > 0 && e.file_type().is_symlink())
    });

    for entry in walker {
        let entry = match entry {
            Ok(e) => e,
            // A vanished/unreadable subtree under /proc is expected churn; skip.
            Err(_) => continue,
        };
        // is_file() is false for symlinks (we don't follow them) and dirs.
        if !entry.file_type().is_file() {
            continue;
        }
        let Some(key) = path_to_key(entry.path(), base) else {
            continue;
        };
        if let Some(re) = &re
            && !re.is_match(&key)
        {
            continue;
        }
        // Write-only and permission-denied knobs error on read — skip them.
        let Ok(raw) = std::fs::read_to_string(entry.path()) else {
            continue;
        };
        rows.push((key, sanitize(raw.trim())));
    }

    rows.sort_by(|a, b| a.0.cmp(&b.0));

    let stdout = io::stdout();
    let handle = stdout.lock();
    let mut writer = BoundedWriter::new(handle, args.limit);

    let mut wrote_any = false;
    for (key, value) in &rows {
        let line = match args.format {
            Format::Tsv => format!("{key}\t{value}"),
            Format::Json => serde_json::to_string(&json!({ "key": key, "value": value }))?,
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

/// Turn a `/proc/sys/...` path into the dotted sysctl key name.
///
/// Strips the `/proc/sys/` base and replaces the path separator with `.`, so
/// `/proc/sys/net/ipv4/tcp_syncookies` becomes `net.ipv4.tcp_syncookies`.
/// Returns `None` for the base itself or a non-UTF-8 path.
fn path_to_key(path: &Path, base: &Path) -> Option<String> {
    let rel = path.strip_prefix(base).ok()?;
    let s = rel.to_str()?;
    if s.is_empty() {
        return None;
    }
    Some(s.replace(std::path::MAIN_SEPARATOR, "."))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_to_key_dots_the_relative_path() {
        let base = Path::new("/proc/sys");
        assert_eq!(
            path_to_key(Path::new("/proc/sys/net/ipv4/tcp_syncookies"), base).as_deref(),
            Some("net.ipv4.tcp_syncookies")
        );
        assert_eq!(
            path_to_key(Path::new("/proc/sys/kernel/hostname"), base).as_deref(),
            Some("kernel.hostname")
        );
    }

    #[test]
    fn path_to_key_rejects_base_itself() {
        let base = Path::new("/proc/sys");
        assert_eq!(path_to_key(Path::new("/proc/sys"), base), None);
    }

    #[test]
    fn path_to_key_rejects_path_outside_base() {
        let base = Path::new("/proc/sys");
        assert_eq!(path_to_key(Path::new("/etc/passwd"), base), None);
    }
}
