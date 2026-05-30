//! Agent-hook redirect rules for the `config` domain.
//!
//! `yq`/`tomlq FILTER FILE` (two-or-more positionals) and any `plistutil`
//! invocation map to `sak config`. `yq` and `tomlq` are flattened into one row
//! each with the tool name baked into the static message (the registry takes
//! `&'static str`, not a formatted string — same flattening pattern used for
//! `rg`/`ripgrep` in `fs` and the `*sum` tools in `hash`).

use crate::hook::rule::HookRule;

pub const HOOK_RULES: &[HookRule] = &[
    HookRule {
        tool: "yq",
        subcommand: &[],
        guard: Some(yq_has_file),
        message: "Use `sak config query <path> <file>` instead of `yq` for files \
             (omit <file> and pass `--format yaml|toml|json|plist` to read stdin). \
             Handles TOML/YAML/JSON/plist.",
    },
    HookRule {
        tool: "tomlq",
        subcommand: &[],
        guard: Some(yq_has_file),
        message: "Use `sak config query <path> <file>` instead of `tomlq` for files \
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

/// `yq`/`tomlq FILTER FILE` has two or more positionals; filter-only
/// invocations and `... | yq .` pipes read stdin and aren't redirected.
fn yq_has_file(args: &[String]) -> bool {
    args.iter().filter(|a| !a.starts_with('-')).count() >= 2
}

#[cfg(test)]
mod tests {
    use super::*;

    fn a(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn yq_guard_distinguishes_file_from_stdin() {
        assert!(yq_has_file(&a(&[".name", "pkg.yaml"])));
        assert!(yq_has_file(&a(&[".package.name", "Cargo.toml"])));
        assert!(!yq_has_file(&a(&["."])));
        assert!(!yq_has_file(&a(&[])));
    }
}
