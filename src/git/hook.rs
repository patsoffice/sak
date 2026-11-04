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
    // `shortlog` splits two ways (first match wins): the count forms
    // (`-s`/`-n`, the common `-sn`) are `sak git contributors`; the bare form
    // is `sak git shortlog`. Anything else (`-e`, `--group`, a revision
    // range) has no sak equivalent and passes through.
    HookRule {
        tool: "git",
        subcommand: &[&["shortlog"]],
        guard: Some(git_shortlog_wants_counts),
        message: "Use `sak git contributors` (commit counts per author) instead of \
             `git shortlog -sn`.",
    },
    HookRule {
        tool: "git",
        subcommand: &[&["shortlog"]],
        guard: Some(no_extra_args),
        message: "Use `sak git shortlog` (-c, --limit supported) instead of `git shortlog`.",
    },
    HookRule {
        tool: "git",
        subcommand: &[&["rev-parse"]],
        guard: Some(git_rev_parse_is_single_ref),
        message: "Use `sak git ref-resolve <ref>` instead of `git rev-parse <ref>`.",
    },
    HookRule {
        tool: "git",
        subcommand: &[&["merge-base"]],
        guard: Some(git_merge_base_is_two_refs),
        message: "Use `sak git merge-base <ref1> <ref2>` instead of `git merge-base <ref1> <ref2>`.",
    },
    HookRule {
        tool: "git",
        subcommand: &[&["ls-files"]],
        guard: Some(no_extra_args),
        message: "Use `sak git ls-files` instead of `git ls-files`.",
    },
    HookRule {
        tool: "git",
        subcommand: &[&["cat-file"]],
        guard: Some(git_cat_file_is_pretty_print),
        message: "Use `sak git cat-file <ref>` instead of `git cat-file -p <ref>` \
             (blobs, tags, trees; for a commit use `sak git show <ref>`).",
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
    HookRule {
        tool: "git",
        subcommand: &[&["grep"]],
        guard: Some(git_grep_searches_worktree),
        message: "Use `sak fs grep <pattern> [path]` instead of `git grep` to \
             search the working tree (-i, -w, -l, -c, --glob work; it skips .git/target/node_modules, and --hidden adds \
             dotfiles). To search a revision or branch, name it before `--`: \
             `git grep <pattern> <rev> -- [path]`.",
    },
];

/// `git grep` over the working tree — what `sak fs grep` searches. Revisions,
/// the index (`--cached`), and pager mode (`-O`) are git-only and pass.
/// Without `--`, git tells a revision from a path by resolving it, which a
/// hook can't; so an argument counts as a revision when it sits between the
/// pattern and an explicit `--`, or when its syntax says so (`HEAD~1`,
/// `a..b`, `rev:path`, `@{u}`, a 7-40 digit hex id). A bare branch name
/// without `--` reads as a path, and the message says how to pass one.
fn git_grep_searches_worktree(args: &[String]) -> bool {
    const VALUE_FLAGS: &[&str] = &[
        "-A",
        "-B",
        "-C",
        "-m",
        "--after-context",
        "--before-context",
        "--context",
        "--max-count",
        "--max-depth",
        "--threads",
    ];
    let rest = &args[1..];
    if rest.is_empty() {
        return false;
    }
    let mut pattern_from_flag = false;
    let mut before_dashdash: Vec<&str> = Vec::new();
    let mut saw_dashdash = false;
    let mut i = 0;
    while i < rest.len() {
        let a = rest[i].as_str();
        i += 1;
        if saw_dashdash {
            continue;
        }
        match a {
            "--" => saw_dashdash = true,
            "--cached" | "--recurse-submodules" => return false,
            _ if a == "-O" || a.starts_with("--open-files-in-pager") => return false,
            "-e" | "-f" | "--regexp" | "--file" => {
                pattern_from_flag = true;
                i += 1;
            }
            _ if a.starts_with("--regexp=") || a.starts_with("--file=") => {
                pattern_from_flag = true;
            }
            _ if VALUE_FLAGS.contains(&a) => i += 1,
            _ if a.starts_with("--") => {}
            _ if a.starts_with('-') && a.len() > 1 => {
                // A short cluster ending in a value flag takes the next arg
                // (`-ie foo`, `-nC 3`); attached values (`-C3`) don't.
                if a.ends_with(['e', 'f']) {
                    pattern_from_flag = true;
                    i += 1;
                } else if a.ends_with(['A', 'B', 'C', 'm']) {
                    i += 1;
                }
            }
            _ => before_dashdash.push(a),
        }
    }
    let skip = usize::from(!pattern_from_flag);
    let revs = before_dashdash.get(skip..).unwrap_or(&[]);
    if saw_dashdash {
        return revs.is_empty();
    }
    !revs.iter().any(|r| looks_like_revision(r))
}

/// Syntax that only a revision has: `HEAD`-family names, `~`/`^` ancestry,
/// `..` ranges, `@{…}` reflog/upstream, `rev:path` tree-ish paths, and
/// abbreviated or full hex object ids.
fn looks_like_revision(arg: &str) -> bool {
    const REFS: &[&str] = &["HEAD", "FETCH_HEAD", "ORIG_HEAD", "MERGE_HEAD"];
    REFS.iter().any(|r| arg.starts_with(r))
        || arg.contains(['~', '^', ':'])
        || arg.contains("..")
        || arg.contains("@{")
        || ((7..=40).contains(&arg.len()) && arg.chars().all(|c| c.is_ascii_hexdigit()))
}

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

/// `git rev-parse <ref>`: exactly one positional that isn't a flag.
/// Flag-only probes (`--git-dir`, `--is-inside-work-tree`) and multi-arg
/// plumbing (`--short HEAD`, `--verify HEAD --`) pass through to real git —
/// only the bare single-ref form maps onto `sak git ref-resolve`.
fn git_rev_parse_is_single_ref(args: &[String]) -> bool {
    let rest = &args[1..];
    rest.len() == 1 && !rest[0].starts_with('-')
}

/// `git cat-file -p <ref>`: the pretty-print dump `sak git cat-file` mirrors.
/// `-t` (type) and `-e` (existence, answered by exit code) are different
/// questions sak doesn't answer, and the batch protocols (`--batch*`) read
/// many objects; all of them pass through to real git.
fn git_cat_file_is_pretty_print(args: &[String]) -> bool {
    let rest = &args[1..];
    rest.len() == 2 && rest[0] == "-p" && !rest[1].starts_with('-')
}

/// `git merge-base <ref1> <ref2>`: exactly two refs, no flags. The flag forms
/// (`--is-ancestor`, an exit-code test; `--fork-point`; `--all`;
/// `--octopus`) and other arities have no sak equivalent and pass through.
fn git_merge_base_is_two_refs(args: &[String]) -> bool {
    let rest = &args[1..];
    rest.len() == 2 && rest.iter().all(|a| !a.starts_with('-'))
}

/// The subcommand with nothing after it. Used where the sak command takes
/// none of git's arguments (`sak git ls-files` has no pathspecs or
/// `--others`/`-m` modes), so only the bare form can redirect.
fn no_extra_args(args: &[String]) -> bool {
    args.len() == 1
}

/// `git shortlog` asking for counts rather than subjects: `--summary` /
/// `--numbered`, or a short-flag cluster containing `s` or `n` (`-s`, `-sn`,
/// `-sne`). That is `sak git contributors`, not `sak git shortlog`.
fn git_shortlog_wants_counts(args: &[String]) -> bool {
    args[1..].iter().any(|a| {
        matches!(a.as_str(), "--summary" | "--numbered")
            || (a.starts_with('-')
                && !a.starts_with("--")
                && a[1..].chars().any(|c| c == 's' || c == 'n'))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn a(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn git_grep_guard_splits_worktree_from_revisions() {
        // Working-tree searches redirect.
        assert!(git_grep_searches_worktree(&a(&["grep", "foo"])));
        assert!(git_grep_searches_worktree(&a(&[
            "grep", "-n", "foo", "src/"
        ])));
        assert!(git_grep_searches_worktree(&a(&[
            "grep", "-e", "foo", "src/"
        ])));
        assert!(git_grep_searches_worktree(&a(&["grep", "-C", "3", "foo"])));
        assert!(git_grep_searches_worktree(&a(&[
            "grep", "foo", "--", "src/"
        ])));
        assert!(git_grep_searches_worktree(&a(&[
            "grep",
            "--no-index",
            "foo"
        ])));
        // Revisions, the index, and pager mode pass through.
        assert!(!git_grep_searches_worktree(&a(&["grep", "foo", "HEAD~1"])));
        assert!(!git_grep_searches_worktree(&a(&[
            "grep",
            "foo",
            "main..dev"
        ])));
        assert!(!git_grep_searches_worktree(&a(&[
            "grep", "foo", "v1.2:src"
        ])));
        assert!(!git_grep_searches_worktree(&a(&["grep", "foo", "a1b2c3d"])));
        assert!(!git_grep_searches_worktree(&a(&[
            "grep", "foo", "main", "--", "src/"
        ])));
        assert!(!git_grep_searches_worktree(&a(&[
            "grep", "-e", "foo", "main", "--"
        ])));
        assert!(!git_grep_searches_worktree(&a(&[
            "grep", "--cached", "foo"
        ])));
        assert!(!git_grep_searches_worktree(&a(&["grep", "-O", "foo"])));
        assert!(!git_grep_searches_worktree(&a(&["grep"])));
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
    fn rev_parse_single_ref_matches_plumbing_doesnt() {
        assert!(git_rev_parse_is_single_ref(&a(&["rev-parse", "HEAD"])));
        assert!(git_rev_parse_is_single_ref(&a(&["rev-parse", "main"])));
        // Flag-only probes and multi-arg plumbing decline.
        assert!(!git_rev_parse_is_single_ref(&a(&["rev-parse"])));
        assert!(!git_rev_parse_is_single_ref(&a(&[
            "rev-parse",
            "--git-dir"
        ])));
        assert!(!git_rev_parse_is_single_ref(&a(&[
            "rev-parse",
            "--short",
            "HEAD"
        ])));
        assert!(!git_rev_parse_is_single_ref(&a(&[
            "rev-parse",
            "HEAD",
            "--"
        ])));
    }

    #[test]
    fn cat_file_pretty_print_matches_other_modes_dont() {
        assert!(git_cat_file_is_pretty_print(&a(&[
            "cat-file",
            "-p",
            "HEAD:README.md"
        ])));
        // `-t` (type) and `-e` (exit-code existence test) aren't what
        // `sak git cat-file` answers; batch and bare forms decline too.
        for args in [
            &["cat-file", "-t", "HEAD"][..],
            &["cat-file", "-e", "HEAD"][..],
            &["cat-file", "--batch"][..],
            &["cat-file", "--batch-check"][..],
            &["cat-file", "-p"][..],
            &["cat-file", "HEAD"][..],
        ] {
            assert!(!git_cat_file_is_pretty_print(&a(args)), "{args:?}");
        }
    }

    #[test]
    fn merge_base_two_refs_matches_flag_forms_dont() {
        assert!(git_merge_base_is_two_refs(&a(&[
            "merge-base",
            "main",
            "HEAD"
        ])));
        for args in [
            &["merge-base", "--is-ancestor", "main", "HEAD"][..],
            &["merge-base", "--fork-point", "main"][..],
            &["merge-base", "--all", "main", "HEAD"][..],
            &["merge-base", "main"][..],
            &["merge-base", "a", "b", "c"][..],
        ] {
            assert!(!git_merge_base_is_two_refs(&a(args)), "{args:?}");
        }
    }

    #[test]
    fn shortlog_count_forms_are_detected() {
        for args in [
            &["shortlog", "-sn"][..],
            &["shortlog", "-s"][..],
            &["shortlog", "-n", "-e"][..],
            &["shortlog", "-sne", "HEAD"][..],
            &["shortlog", "--summary"][..],
            &["shortlog", "--numbered"][..],
        ] {
            assert!(git_shortlog_wants_counts(&a(args)), "{args:?}");
        }
        assert!(!git_shortlog_wants_counts(&a(&["shortlog"])));
        assert!(!git_shortlog_wants_counts(&a(&["shortlog", "-e"])));
        assert!(!git_shortlog_wants_counts(&a(&["shortlog", "--email"])));
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
