//! Declarative hook-rule format shared by every domain's `HOOK_RULES` table.
//!
//! A [`HookRule`] says: when the user runs `tool` with one of these
//! `subcommand` positional prefixes (and an optional `guard` over the full args
//! holds), redirect to a `sak` equivalent with `message`. The claude-code
//! matching engine in [`super::claude_code`] aggregates every domain's table
//! via its `registries()` and consults them before its legacy per-tool
//! `check_*` fallback, so domains can migrate one at a time.

/// One redirect rule, owned by the domain whose commands shadow `tool`.
pub struct HookRule {
    /// Command basename to match (e.g. `"git"`, `"kubectl"`).
    pub tool: &'static str,
    /// Accepted positional-verb prefixes. Each inner slice is a sequence that
    /// must match the leading positionals in order; the outer slice is a set of
    /// alternatives (OR). An empty outer slice matches any invocation of `tool`
    /// (used by single-purpose tools like `tree` that have no subcommand).
    pub subcommand: &'static [&'static [&'static str]],
    /// Optional extra predicate over the full (normalized) argument list. When
    /// present it must return `true` for the rule to fire — used for
    /// conditional redirects (git's list-only `branch`, GET-only `gh api`, the
    /// pure-only `nix eval`, ...).
    pub guard: Option<fn(&[String]) -> bool>,
    /// Redirect message shown to the model. The engine appends the bypass hint.
    pub message: &'static str,
}

/// Whether `pos` (the positional args) satisfies a rule's `subcommand`
/// alternatives: an empty alternative set matches any invocation, otherwise one
/// alternative must be a prefix of `pos`.
pub fn subcommand_matches(alternatives: &[&[&str]], pos: &[&str]) -> bool {
    if alternatives.is_empty() {
        return true;
    }
    alternatives
        .iter()
        .any(|seq| seq.len() <= pos.len() && seq.iter().zip(pos.iter()).all(|(a, b)| a == b))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_alternatives_match_anything() {
        assert!(subcommand_matches(&[], &[]));
        assert!(subcommand_matches(&[], &["whatever"]));
    }

    #[test]
    fn single_verb_prefix_matches() {
        assert!(subcommand_matches(&[&["status"]], &["status"]));
        // Extra positionals after the matched prefix are fine.
        assert!(subcommand_matches(&[&["status"]], &["status", "--short"]));
        assert!(!subcommand_matches(&[&["status"]], &["log"]));
        assert!(!subcommand_matches(&[&["status"]], &[]));
    }

    #[test]
    fn multi_verb_sequence_matches_in_order() {
        assert!(subcommand_matches(&[&["repo", "list"]], &["repo", "list"]));
        assert!(!subcommand_matches(&[&["repo", "list"]], &["repo", "add"]));
        // A partial prefix (only the first verb present) does not match a
        // two-verb sequence.
        assert!(!subcommand_matches(&[&["repo", "list"]], &["repo"]));
    }

    #[test]
    fn any_alternative_can_match() {
        let alts: &[&[&str]] = &[&["list"], &["ls"]];
        assert!(subcommand_matches(alts, &["list"]));
        assert!(subcommand_matches(alts, &["ls"]));
        assert!(!subcommand_matches(alts, &["status"]));
    }
}
