//! Agent-hook redirect rules for the `git` domain.
//!
//! Read-only git subcommands (`status`, `diff`, `log`, `show`, `blame`,
//! `shortlog`, `stash list`) redirect unconditionally; the three multi-mode
//! verbs (`branch`, `tag`, `remote`) redirect only their *listing* forms via
//! per-rule guards — the modifying forms (`git branch -D foo`, `git tag -a v1`,
//! `git remote add origin …`) pass through because `sak git` deliberately
//! can't perform them.
//!
//! Global flag stripping (`git -C /tmp status`, `git --git-dir … log`, …) is
//! the engine's job: [`crate::hook::claude_code::normalize_args`] runs
//! [`crate::hook::claude_code::strip_git_global_flags`] for `tool == "git"`
//! before the subcommand/guard match, so this file never sees the globals.

use crate::hook::rule::HookRule;

pub const HOOK_RULES: &[HookRule] = &[
    HookRule {
        tool: "git",
        subcommand: &[&["status"]],
        guard: None,
        message: "Use `sak git status` instead of `git status`.",
    },
    HookRule {
        tool: "git",
        subcommand: &[&["diff"]],
        guard: None,
        message: "Use `sak git diff` (--staged, --name-only, --stat, --commit supported) \
             instead of `git diff`.",
    },
    HookRule {
        tool: "git",
        subcommand: &[&["log"]],
        guard: None,
        message: "Use `sak git log` (--oneline, -n, --author, --grep, --since, -- <path> supported) \
             instead of `git log`.",
    },
    HookRule {
        tool: "git",
        subcommand: &[&["show"]],
        guard: None,
        message: "Use `sak git show` (--stat, --name-only, --format supported) \
             instead of `git show`.",
    },
    HookRule {
        tool: "git",
        subcommand: &[&["blame"]],
        guard: None,
        message: "Use `sak git blame` (-L 10,20 supported) instead of `git blame`.",
    },
    HookRule {
        tool: "git",
        subcommand: &[&["shortlog"]],
        guard: None,
        message: "Use `sak git contributors` instead of `git shortlog`.",
    },
    HookRule {
        tool: "git",
        subcommand: &[&["branch"]],
        guard: Some(git_branch_is_listing),
        message: "Use `sak git branch` to list branches. \
             (`git branch -d/-D/-m/-c/<name>` is allowed.)",
    },
    HookRule {
        tool: "git",
        subcommand: &[&["tag"]],
        guard: Some(git_tag_is_listing),
        message: "Use `sak git tags` to list tags. \
             (`git tag -a/-d <name>` is allowed.)",
    },
    HookRule {
        tool: "git",
        subcommand: &[&["remote"]],
        guard: Some(git_remote_is_listing),
        message: "Use `sak git remote` to list remotes. \
             (`git remote add/remove/set-url` is allowed.)",
    },
    HookRule {
        tool: "git",
        subcommand: &[&["stash", "list"]],
        guard: None,
        message: "Use `sak git stash-list` instead of `git stash list`.",
    },
];

/// `git branch` in a listing form: no extra args, or only the list-flavor flags
/// (`-a`/`--all`, `-r`/`--remotes`, `-l`/`--list`, `-v`/`-vv`/`--verbose`,
/// `--show-current`). Any positional (a branch name) or modifying flag
/// (`-d`/`-D`/`-m`/`-c`) takes it out of listing territory.
fn git_branch_is_listing(args: &[String]) -> bool {
    const LIST_FLAGS: &[&str] = &[
        "-a",
        "--all",
        "-r",
        "--remotes",
        "-l",
        "--list",
        "-v",
        "-vv",
        "--verbose",
        "--show-current",
    ];
    let rest = &args[1..];
    rest.is_empty() || rest.iter().all(|a| LIST_FLAGS.contains(&a.as_str()))
}

/// `git tag` in a listing form: no extra args, or only the list-flavor flags
/// (`-l`/`--list`, `-n[N]`, `--column`/`--no-column`, `--sort=…`). Any
/// positional (a tag name) or modifying flag (`-a`/`-d`) declines.
fn git_tag_is_listing(args: &[String]) -> bool {
    let rest = &args[1..];
    rest.is_empty()
        || rest.iter().all(|a| {
            matches!(
                a.as_str(),
                "-l" | "--list" | "-n" | "--column" | "--no-column"
            ) || a.starts_with("-n")
                || a.starts_with("--sort")
        })
}

/// `git remote` in a listing form: no extra args, or one of the read subverbs
/// (`-v`/`--verbose`, `show`, `get-url`). `add`/`remove`/`rename`/`set-url`
/// decline so they pass through to real git.
fn git_remote_is_listing(args: &[String]) -> bool {
    let rest = &args[1..];
    rest.is_empty()
        || matches!(
            rest.first().map(String::as_str),
            Some("-v" | "--verbose" | "show" | "get-url")
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn a(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn branch_listing_forms_match_modifying_forms_dont() {
        assert!(git_branch_is_listing(&a(&["branch"])));
        assert!(git_branch_is_listing(&a(&["branch", "-a"])));
        assert!(git_branch_is_listing(&a(&["branch", "--all"])));
        assert!(git_branch_is_listing(&a(&["branch", "-r", "--verbose"])));
        // A positional (branch name) or modifying flag declines.
        assert!(!git_branch_is_listing(&a(&["branch", "-D", "feature/old"])));
        assert!(!git_branch_is_listing(&a(&["branch", "-m", "old", "new"])));
        assert!(!git_branch_is_listing(&a(&["branch", "new-branch"])));
    }

    #[test]
    fn tag_listing_forms_match_modifying_forms_dont() {
        assert!(git_tag_is_listing(&a(&["tag"])));
        assert!(git_tag_is_listing(&a(&["tag", "-l"])));
        assert!(git_tag_is_listing(&a(&["tag", "--list"])));
        assert!(git_tag_is_listing(&a(&["tag", "--sort=-creatordate"])));
        assert!(git_tag_is_listing(&a(&["tag", "-n5"])));
        assert!(!git_tag_is_listing(&a(&["tag", "-a", "v1.0", "-m", "hi"])));
        assert!(!git_tag_is_listing(&a(&["tag", "v1.0"])));
        assert!(!git_tag_is_listing(&a(&["tag", "-d", "v0.9"])));
    }

    #[test]
    fn remote_listing_forms_match_modifying_forms_dont() {
        assert!(git_remote_is_listing(&a(&["remote"])));
        assert!(git_remote_is_listing(&a(&["remote", "-v"])));
        assert!(git_remote_is_listing(&a(&["remote", "show", "origin"])));
        assert!(git_remote_is_listing(&a(&["remote", "get-url", "origin"])));
        assert!(!git_remote_is_listing(&a(&[
            "remote", "add", "origin", "url"
        ])));
        assert!(!git_remote_is_listing(&a(&["remote", "remove", "origin"])));
        assert!(!git_remote_is_listing(&a(&[
            "remote", "set-url", "origin", "url"
        ])));
    }
}
