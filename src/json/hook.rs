//! Agent-hook redirect rules for the `json` domain.
//!
//! `jq FILTER [FILE]` maps to `sak json query`. Both the file form
//! (`jq FILTER FILE`) and the stdin form (`jq FILTER`, `... | jq .`) redirect —
//! `sak json query` reads stdin via `-`, so a piped filter is just as
//! redirectable as a file argument. Only a bare `jq` (no filter) passes through.

use crate::hook::rule::HookRule;

pub const HOOK_RULES: &[HookRule] = &[HookRule {
    tool: "jq",
    subcommand: &[],
    guard: Some(jq_has_filter),
    message: "Use `sak json query <path> <file>` instead of `jq` \
         (pass `-` as <file> to read stdin, e.g. `cmd | sak json query <path> -`). \
         Other ops: keys, flatten, grep, length, paths, schema, type, validate, diff.",
}];

/// `jq FILTER [FILE]` carries a filter positional whether it reads a file or
/// stdin, so any invocation with at least one positional redirects. A bare `jq`
/// (no filter) has nothing to redirect.
fn jq_has_filter(args: &[String]) -> bool {
    args.iter().any(|a| !a.starts_with('-'))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn a(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn jq_guard_fires_on_any_filter() {
        // File reads still fire.
        assert!(jq_has_filter(&a(&[".name", "pkg.json"])));
        assert!(jq_has_filter(&a(&["-r", ".name", "pkg.json"])));
        // Filter-only (stdin) invocations now fire too — sak reads stdin via `-`.
        assert!(jq_has_filter(&a(&["."])));
        assert!(jq_has_filter(&a(&["-r", "."])));
        // A bare `jq` (no filter) has nothing to redirect.
        assert!(!jq_has_filter(&a(&[])));
        assert!(!jq_has_filter(&a(&["-r"])));
    }
}
