//! Shared output helpers for the prom domain.
//!
//! Every `sak prom` command supports `--json` (a raw pretty-printed
//! passthrough of the upstream response) and most render free-text fields
//! that may contain newlines. The `--json` `BoundedWriter` dance now lives in
//! [`crate::output::emit_json`] (shared with `k8s`/`lxc`/`docker`) and is
//! re-exported below; the newline-collapsing logic, which is prom-specific,
//! lives here.

// The JSON `--json` dump shared by every `sak prom` command now lives in
// `crate::output` alongside `BoundedWriter`, where `sak k8s schema`,
// `sak docker info`, and `sak lxc info` share it too. Re-exported here so the
// prom command files keep importing it from their own domain's output module.
pub(super) use crate::output::emit_json;

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
