//! Shared output helpers for the prom domain.
//!
//! Every `sak prom` command supports `--json` (a raw pretty-printed
//! passthrough of the upstream response) and most render free-text fields
//! that may contain newlines. Rather than duplicate the `BoundedWriter`
//! dance and the newline-collapsing logic across every command file, they
//! live here once.

use std::io;
use std::process::ExitCode;

use anyhow::Result;
use serde_json::Value;

use crate::output::BoundedWriter;

/// Pretty-print `data` as JSON through a [`BoundedWriter`], honoring
/// `--limit`. This is the `--json` branch shared by every `sak prom`
/// command. Always returns [`ExitCode::SUCCESS`] — a `--json` dump of an
/// empty result is still a successful response, just an empty one.
pub(super) fn emit_json(data: &Value, limit: Option<usize>) -> Result<ExitCode> {
    let stdout = io::stdout();
    let handle = stdout.lock();
    let mut writer = BoundedWriter::new(handle, limit);
    let pretty = serde_json::to_string_pretty(data)?;
    for line in pretty.lines() {
        if !writer.write_line(line)? {
            break;
        }
    }
    writer.flush()?;
    Ok(ExitCode::SUCCESS)
}

/// Collapse `\n` and `\r` in `s` to spaces so a multi-line free-text field
/// (alert summary, target `lastError`, rule `query`, ...) stays on one
/// output row. Implemented via `chars().map()` rather than `str::replace`
/// to match the `k8s::events::collapse_newlines` style.
pub(super) fn collapse_newlines(s: &str) -> String {
    s.chars()
        .map(|c| if c == '\n' || c == '\r' { ' ' } else { c })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collapse_newlines_replaces_cr_and_lf() {
        assert_eq!(
            collapse_newlines("line1\nline2\rline3\r\nline4"),
            "line1 line2 line3  line4"
        );
    }

    #[test]
    fn collapse_newlines_leaves_clean_string_untouched() {
        assert_eq!(collapse_newlines("no newlines here"), "no newlines here");
    }
}
