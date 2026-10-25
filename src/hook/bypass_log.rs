//! Append-only log of inline `SAK_HOOK_BYPASS` requests.
//!
//! Each time a model writes the bypass marker on a command a rule would have
//! blocked, the hook appends one JSON line: when, which harness, where, the
//! command, and the redirect it was trying to skip. Reading the log shows
//! which sak gaps push models toward the escape hatch, so they can be closed.
//!
//! This is the one place sak writes to disk, and only to its own state file.
//! It's best-effort: a log that can't be written never changes the hook's
//! decision.

use std::ffi::OsString;
use std::fs::OpenOptions;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

/// `$XDG_STATE_HOME/sak/hook-bypass.jsonl`, else
/// `$HOME/.local/state/sak/hook-bypass.jsonl`; `None` when neither is set.
pub(crate) fn default_path() -> Option<PathBuf> {
    path_from(std::env::var_os("XDG_STATE_HOME"), std::env::var_os("HOME"))
}

/// [`default_path`] over explicit values, for tests. The XDG spec says a
/// relative `XDG_STATE_HOME` is invalid and must be ignored.
fn path_from(xdg_state: Option<OsString>, home: Option<OsString>) -> Option<PathBuf> {
    let base = match xdg_state.map(PathBuf::from) {
        Some(x) if x.is_absolute() => x,
        _ => PathBuf::from(home.filter(|h| !h.is_empty())?).join(".local/state"),
    };
    Some(base.join("sak").join("hook-bypass.jsonl"))
}

/// One log record as a JSON line (no trailing newline).
fn entry(ts: u64, harness: &str, cwd: Option<&Path>, command: &str, rule: &str) -> String {
    serde_json::json!({
        "ts": ts,
        "harness": harness,
        "cwd": cwd.map(|p| p.display().to_string()),
        "command": command,
        "rule": rule,
    })
    .to_string()
}

/// Append one record to `path`, creating its directory as needed. The line
/// goes out in a single write, so concurrent hooks don't interleave.
pub(crate) fn append(path: &Path, harness: &str, command: &str, rule: &str) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let cwd = std::env::current_dir().ok();
    let line = entry(ts, harness, cwd.as_deref(), command, rule);
    let mut file = OpenOptions::new().create(true).append(true).open(path)?;
    file.write_all(format!("{line}\n").as_bytes())
}

/// Log a bypass request to the default path, ignoring any failure.
pub(crate) fn record(harness: &str, command: &str, rule: &str) {
    if let Some(path) = default_path() {
        let _ = append(&path, harness, command, rule);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_prefers_absolute_xdg_state_home() {
        assert_eq!(
            path_from(Some("/x/state".into()), Some("/home/u".into())),
            Some(PathBuf::from("/x/state/sak/hook-bypass.jsonl"))
        );
        // Unset, empty, or relative XDG_STATE_HOME falls back to HOME.
        let fallback = Some(PathBuf::from("/home/u/.local/state/sak/hook-bypass.jsonl"));
        assert_eq!(path_from(None, Some("/home/u".into())), fallback);
        assert_eq!(path_from(Some("".into()), Some("/home/u".into())), fallback);
        assert_eq!(
            path_from(Some("rel".into()), Some("/home/u".into())),
            fallback
        );
        assert_eq!(path_from(None, None), None);
        assert_eq!(path_from(None, Some("".into())), None);
    }

    #[test]
    fn append_writes_one_json_line_per_request() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested/sak/hook-bypass.jsonl");
        append(
            &path,
            "claude-code",
            "SAK_HOOK_BYPASS=1 git status",
            "Use `sak git status`.",
        )
        .unwrap();
        append(
            &path,
            "muse-code",
            "SAK_HOOK_BYPASS=1 cat a\tb",
            "Use `sak fs read`.",
        )
        .unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<serde_json::Value> = text
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0]["harness"], "claude-code");
        assert_eq!(lines[0]["command"], "SAK_HOOK_BYPASS=1 git status");
        assert_eq!(lines[0]["rule"], "Use `sak git status`.");
        assert!(lines[0]["ts"].as_u64().unwrap() > 0);
        // Tabs and the like stay inside the JSON string, one record per line.
        assert_eq!(lines[1]["command"], "SAK_HOOK_BYPASS=1 cat a\tb");
    }
}
