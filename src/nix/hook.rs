//! Agent-hook redirect rules for the `nix` domain.
//!
//! Two binaries — `nix` (the new hierarchical CLI: `nix flake show`, `nix
//! store info`, …) and `nix-store` (the classic stable CLI; sak only shadows
//! its `--query` reads). The rule rows for `nix` cover read subverbs and a
//! handful of deprecated aliases (`nix flake info` ≡ `metadata`, `nix store
//! ping` ≡ `info`, `nix show-derivation` ≡ `derivation show`); the rows for
//! `nix-store` use empty `subcommand` + guards that scan the args for
//! `--query` paired with the right sub-flag, since the discrimination is
//! flag-driven rather than verb-driven.
//!
//! These rules are also why `src/nix/client.rs`'s chokepoint test had to be
//! split: the bare token list (`"nix"`, `"nix-store"`) is now asserted via
//! [`crate::test_support::assert_no_forbidden_tokens_except`] with `hook.rs`
//! exempted (it legitimately holds those strings as rule `tool` fields), while
//! `Command::new(` stays in the strict 3-arg form — hook rules never spawn
//! subprocesses.

use crate::hook::rule::HookRule;

pub const HOOK_RULES: &[HookRule] = &[
    // ── nix ────────────────────────────────────────────────────────────
    HookRule {
        tool: "nix",
        subcommand: &[&["flake", "show"]],
        guard: None,
        message: "Use `sak nix flake-show [flake-ref]` instead of `nix flake show` \
             (TSV output-path/type/description, --all-systems, --format json).",
    },
    // `nix flake info` is the deprecated alias for `nix flake metadata`.
    HookRule {
        tool: "nix",
        subcommand: &[&["flake", "metadata"], &["flake", "info"]],
        guard: None,
        message: "Use `sak nix flake-metadata [flake-ref]` instead of `nix flake metadata`/`info` \
             (TSV locked.rev/lastModified/narHash/original.url/path, --field, --format json).",
    },
    // `nix store ping` is the deprecated alias for `nix store info`.
    HookRule {
        tool: "nix",
        subcommand: &[&["store", "info"], &["store", "ping"]],
        guard: None,
        message: "Use `sak nix store-info` instead of `nix store info`/`nix store ping` \
             (TSV url/version/trusted/..., --field, --store, --format json).",
    },
    // Only `registry list` is a read; `registry add`/`remove`/`pin` mutate
    // and pass through (sak nix can't perform them).
    HookRule {
        tool: "nix",
        subcommand: &[&["registry", "list"]],
        guard: None,
        message: "Use `sak nix registry-list` instead of `nix registry list` \
             (TSV scope/from/to, --scope, --format json).",
    },
    // Only `profile list` is a read; install/remove/upgrade/rollback/
    // wipe-history all mutate the profile and pass through.
    HookRule {
        tool: "nix",
        subcommand: &[&["profile", "list"]],
        guard: None,
        message: "Use `sak nix profile-list` instead of `nix profile list` \
             (TSV index/name/store-path/flake-attr, --profile, --format json).",
    },
    // `nix derivation show` and the deprecated top-level `nix show-derivation`
    // both read; `nix derivation add` mutates and passes through.
    HookRule {
        tool: "nix",
        subcommand: &[&["derivation", "show"], &["show-derivation"]],
        guard: None,
        message: "Use `sak nix derivation-show [installable]` instead of `nix derivation show` \
             (JSON passthrough, --recursive).",
    },
    // `nix path-info` is always a read (no mutating subcommand).
    HookRule {
        tool: "nix",
        subcommand: &[&["path-info"]],
        guard: None,
        message: "Use `sak nix path-info <path...>` instead of `nix path-info` \
             (TSV path/nar_size/closure_size/deriver/signatures, --closure, --format json).",
    },
    // `nix eval` only redirects in the *pure* case — `sak nix eval` injects
    // `--read-only`, which would change `--impure`/`--no-pure-eval` semantics.
    HookRule {
        tool: "nix",
        subcommand: &[&["eval"]],
        guard: Some(nix_eval_is_pure),
        message: "Use `sak nix eval [installable] [--expr <e>] [-f <file>]` instead of `nix eval` \
             (read-only, --json/--raw, --apply).",
    },
    // ── nix-store ──────────────────────────────────────────────────────
    // `--query` is the only read mode; the guards check it AND the right
    // sub-flag together. The two rules cover disjoint sub-flag sets so order
    // doesn't matter; both rules use empty `subcommand` because nix-store has
    // no verb to prefix-match against — the flag form is the verb.
    HookRule {
        tool: "nix-store",
        subcommand: &[],
        guard: Some(nix_store_query_is_refs),
        message: "Use `sak nix references <path>` (--referrers / --closure) instead of \
             `nix-store --query --references/--referrers/--requisites`.",
    },
    HookRule {
        tool: "nix-store",
        subcommand: &[],
        guard: Some(nix_store_query_is_info_size),
        message: "Use `sak nix path-info <path...>` instead of `nix-store --query --info`/`-S` \
             (TSV path/nar_size/closure_size/deriver/signatures, --closure, --format json).",
    },
];

/// `nix eval` is a pure evaluation: no `--impure` or `--no-pure-eval`. The
/// `sak nix eval` chokepoint injects `--read-only` unconditionally, which
/// would change the semantics of an impure eval, so those forms pass through
/// to the user's `nix`.
fn nix_eval_is_pure(args: &[String]) -> bool {
    !args
        .iter()
        .any(|a| a == "--impure" || a == "--no-pure-eval")
}

/// `nix-store --query --references/--referrers/--requisites <path>` — the
/// three reference queries `sak nix references` covers.
fn nix_store_query_is_refs(args: &[String]) -> bool {
    let has = |f: &str| args.iter().any(|a| a == f);
    has("--query") && (has("--references") || has("--referrers") || has("--requisites"))
}

/// `nix-store --query --info`/`-S`/`--size <path>` — the path-metadata
/// queries that overlap with `sak nix path-info`.
fn nix_store_query_is_info_size(args: &[String]) -> bool {
    let has = |f: &str| args.iter().any(|a| a == f);
    has("--query") && (has("--info") || has("-S") || has("--size"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn a(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn eval_guard_excludes_impure_forms() {
        assert!(nix_eval_is_pure(&a(&[
            "eval",
            ".#packages.x86_64-linux.default"
        ])));
        assert!(nix_eval_is_pure(&a(&["eval", "--expr", "1 + 2"])));
        assert!(!nix_eval_is_pure(&a(&[
            "eval",
            "--impure",
            "--expr",
            "builtins.currentTime"
        ])));
        assert!(!nix_eval_is_pure(&a(&["eval", "--no-pure-eval", ".#x"])));
    }

    #[test]
    fn nix_store_query_guards_split_refs_vs_info() {
        // refs guard
        assert!(nix_store_query_is_refs(&a(&[
            "--query",
            "--references",
            "/nix/store/x"
        ])));
        assert!(nix_store_query_is_refs(&a(&[
            "--query",
            "--referrers",
            "/nix/store/x"
        ])));
        assert!(nix_store_query_is_refs(&a(&[
            "--query",
            "--requisites",
            "/nix/store/x"
        ])));
        assert!(!nix_store_query_is_refs(&a(&[
            "--query",
            "--info",
            "/nix/store/x"
        ])));
        assert!(!nix_store_query_is_refs(&a(&[
            "--references",
            "/nix/store/x"
        ]))); // no --query
        // info/size guard
        assert!(nix_store_query_is_info_size(&a(&[
            "--query",
            "--info",
            "/nix/store/x"
        ])));
        assert!(nix_store_query_is_info_size(&a(&[
            "--query",
            "-S",
            "/nix/store/x"
        ])));
        assert!(nix_store_query_is_info_size(&a(&[
            "--query",
            "--size",
            "/nix/store/x"
        ])));
        assert!(!nix_store_query_is_info_size(&a(&[
            "--query",
            "--references",
            "/nix/store/x"
        ])));
        // Other queries (--deriver, --outputs, --tree) and writes (--delete, --gc, --add)
        // hit neither guard.
        assert!(!nix_store_query_is_refs(&a(&[
            "--query",
            "--deriver",
            "/nix/store/x"
        ])));
        assert!(!nix_store_query_is_info_size(&a(&[
            "--query",
            "--outputs",
            "/nix/store/x"
        ])));
        assert!(!nix_store_query_is_refs(&a(&["--delete", "/nix/store/x"])));
        assert!(!nix_store_query_is_info_size(&a(&["--gc"])));
    }
}
