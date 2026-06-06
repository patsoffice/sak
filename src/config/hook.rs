//! Agent-hook redirect rules for the `config` domain.
//!
//! `yq`/`tomlq FILTER [FILE]` and any `plistutil` invocation map to `sak
//! config`. Both the file form and the stdin form (`... | yq .`) redirect — `sak
//! config query` reads stdin when `<file>` is omitted (with `--format`), so a
//! piped filter is just as redirectable as a file argument. `yq` and `tomlq`
//! are flattened into one row each with the tool name baked into the static
//! message (the registry takes `&'static str`, not a formatted string — same
//! flattening pattern used for `rg`/`ripgrep` in `fs` and the `*sum` tools in
//! `hash`).

use crate::hook::rule::HookRule;

pub const HOOK_RULES: &[HookRule] = &[
    HookRule {
        tool: "yq",
        subcommand: &[],
        guard: Some(yq_has_filter),
        message: "Use `sak config query <path> <file>` instead of `yq` \
             (omit <file> and pass `--format yaml|toml|json|plist` to read stdin). \
             Handles TOML/YAML/JSON/plist.",
    },
    HookRule {
        tool: "tomlq",
        subcommand: &[],
        guard: Some(yq_has_filter),
        message: "Use `sak config query <path> <file>` instead of `tomlq` \
             (omit <file> and pass `--format toml|yaml|json|plist` to read stdin). \
             Handles TOML/YAML/JSON/plist.",
    },
    HookRule {
        tool: "plistutil",
        subcommand: &[],
        guard: None,
        message: "Use `sak config query/keys/flatten <file>` instead of `plistutil`.",
    },
];

/// `yq`/`tomlq FILTER [FILE]` carries a filter positional whether it reads a
/// file or stdin, so any invocation with at least one positional redirects. A
/// bare `yq` (no filter) has nothing to redirect.
fn yq_has_filter(args: &[String]) -> bool {
    args.iter().any(|a| !a.starts_with('-'))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn a(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn yq_guard_fires_on_any_filter() {
        // File reads still fire.
        assert!(yq_has_filter(&a(&[".name", "pkg.yaml"])));
        assert!(yq_has_filter(&a(&[".package.name", "Cargo.toml"])));
        // Filter-only (stdin) invocations now fire too.
        assert!(yq_has_filter(&a(&["."])));
        // A bare `yq` (no filter) has nothing to redirect.
        assert!(!yq_has_filter(&a(&[])));
    }
}
