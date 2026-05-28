//! Agent-hook redirect rules for the `gh` domain.
//!
//! Most `gh` reads are noun-verb pairs (`gh pr list`, `gh issue view`, …)
//! that map cleanly to a two-element `subcommand` prefix. The exception is
//! `gh api`, which redirects only its HTTP-GET reads: a non-GET `gh api` is a
//! mutation `sak gh` deliberately can't perform, so the rule carries a
//! [`gh_api_method_is_get`] guard lifted verbatim from the old check_gh. The
//! same method-detection logic lives in [`crate::gh::client::check_api_method`]
//! — keep the two in sync if `gh api`'s flag surface ever changes.
//!
//! Like [`crate::nix::hook`], this file is exempt from `src/gh/client.rs`'s
//! binary-name chokepoint test: `tool: "gh"` and every redirect message
//! mention `"gh"` legitimately. The `Command::new(` half of that chokepoint
//! still applies — hook rules never spawn subprocesses.

use crate::hook::rule::HookRule;

pub const HOOK_RULES: &[HookRule] = &[
    // `gh api` redirects only on HTTP GET. Mutating methods (POST/PUT/PATCH/
    // DELETE) and GraphQL writes are passed through to real gh — sak gh
    // can't perform them.
    HookRule {
        tool: "gh",
        subcommand: &[&["api"]],
        guard: Some(gh_api_method_is_get),
        message: "Use `sak gh api <endpoint>` instead of `gh api` for GET requests.",
    },
    HookRule {
        tool: "gh",
        subcommand: &[&["pr", "list"]],
        guard: None,
        message: "Use `sak gh pr-list` instead of `gh pr list` (TSV/JSON, --fields forwarded).",
    },
    HookRule {
        tool: "gh",
        subcommand: &[&["pr", "view"]],
        guard: None,
        message: "Use `sak gh pr-view <pr>` instead of `gh pr view` (JSON/TSV).",
    },
    HookRule {
        tool: "gh",
        subcommand: &[&["issue", "list"]],
        guard: None,
        message: "Use `sak gh issue-list` instead of `gh issue list` (TSV/JSON, --fields forwarded).",
    },
    HookRule {
        tool: "gh",
        subcommand: &[&["issue", "view"]],
        guard: None,
        message: "Use `sak gh issue-view <issue>` instead of `gh issue view` (JSON/TSV).",
    },
    HookRule {
        tool: "gh",
        subcommand: &[&["run", "list"]],
        guard: None,
        message: "Use `sak gh run-list` instead of `gh run list` (TSV/JSON, --fields forwarded).",
    },
    HookRule {
        tool: "gh",
        subcommand: &[&["run", "view"]],
        guard: None,
        message: "Use `sak gh run-view <run-id>` instead of `gh run view` (JSON/TSV, or --log/--log-failed).",
    },
    HookRule {
        tool: "gh",
        subcommand: &[&["release", "list"]],
        guard: None,
        message: "Use `sak gh release-list` instead of `gh release list` (TSV/JSON, --fields forwarded).",
    },
    HookRule {
        tool: "gh",
        subcommand: &[&["release", "view"]],
        guard: None,
        message: "Use `sak gh release-view [<tag>]` instead of `gh release view` (JSON/TSV).",
    },
    HookRule {
        tool: "gh",
        subcommand: &[&["workflow", "list"]],
        guard: None,
        message: "Use `sak gh workflow-list` instead of `gh workflow list` (TSV/JSON, --fields forwarded).",
    },
    HookRule {
        tool: "gh",
        subcommand: &[&["repo", "view"]],
        guard: None,
        message: "Use `sak gh repo-view [<owner/name>]` instead of `gh repo view` (JSON/TSV).",
    },
];

/// Whether a `gh api` invocation is an HTTP GET — true when no `-X` /
/// `--method` flag is present (gh's default), or its value is `GET`
/// (case-insensitive). Accepts every gh-supported spelling: separated
/// (`-X GET`, `--method GET`), inline (`-XGET`), or `=`-joined
/// (`--method=GET`). Mirrors the same logic in
/// [`crate::gh::client::check_api_method`] — keep them in sync.
fn gh_api_method_is_get(args: &[String]) -> bool {
    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        let method = if a == "-X" || a == "--method" {
            i += 1;
            args.get(i).map(String::as_str)
        } else {
            a.strip_prefix("-X").or_else(|| a.strip_prefix("--method="))
        };
        if let Some(m) = method {
            return m.eq_ignore_ascii_case("GET");
        }
        i += 1;
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn a(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn api_method_defaults_to_get() {
        // No method flag → gh's default GET.
        assert!(gh_api_method_is_get(&a(&["api", "repos/cli/cli"])));
        assert!(gh_api_method_is_get(&a(&[
            "api",
            "graphql",
            "-f",
            "query={ viewer { login } }"
        ])));
    }

    #[test]
    fn api_method_accepts_explicit_get_in_every_spelling() {
        assert!(gh_api_method_is_get(&a(&["api", "x", "-X", "GET"])));
        assert!(gh_api_method_is_get(&a(&["api", "x", "--method", "get"])));
        assert!(gh_api_method_is_get(&a(&["api", "x", "-XGET"])));
        assert!(gh_api_method_is_get(&a(&["api", "x", "--method=GET"])));
    }

    #[test]
    fn api_method_rejects_non_get() {
        assert!(!gh_api_method_is_get(&a(&["api", "x", "-X", "POST"])));
        assert!(!gh_api_method_is_get(&a(&[
            "api", "x", "--method", "DELETE"
        ])));
        assert!(!gh_api_method_is_get(&a(&["api", "x", "-XPATCH"])));
        assert!(!gh_api_method_is_get(&a(&["api", "x", "--method=PUT"])));
    }
}
