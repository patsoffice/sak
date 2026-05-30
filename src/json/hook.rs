//! Agent-hook redirect rules for the `json` domain.
//!
//! `jq FILTER FILE` (two-or-more positionals) maps to `sak json query`; a
//! piped or single-positional invocation is just stdin processing and passes
//! through.

use crate::hook::rule::HookRule;

pub const HOOK_RULES: &[HookRule] = &[HookRule {
    tool: "jq",
    subcommand: &[],
    guard: Some(jq_has_file),
    message: "Use `sak json query <path> <file>` instead of `jq` for files \
         (pass `-` as <file> to read stdin, e.g. `cmd | sak json query <path> -`). \
         Other ops: keys, flatten, grep, length, paths, schema, type, validate, diff.",
}];

/// `jq FILTER FILE` has two or more positionals; `jq FILTER` (stdin) or piped
/// `... | jq .` has at most one and reads stdin — not a redirect.
fn jq_has_file(args: &[String]) -> bool {
    args.iter().filter(|a| !a.starts_with('-')).count() >= 2
}

#[cfg(test)]
mod tests {
    use super::*;

    fn a(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn jq_guard_distinguishes_file_from_stdin() {
        assert!(jq_has_file(&a(&[".name", "pkg.json"])));
        assert!(jq_has_file(&a(&["-r", ".name", "pkg.json"])));
        // Filter-only invocations read stdin.
        assert!(!jq_has_file(&a(&["."])));
        assert!(!jq_has_file(&a(&["-r", "."])));
        assert!(!jq_has_file(&a(&[])));
    }
}
