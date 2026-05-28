//! Agent-hook redirect rules for the `fs` domain.
//!
//! Each row maps a read-only shell tool (`cat`, `head`, `tail`, `grep`, `rg`,
//! `find`, `tree`, `stat`, `wc`) to its `sak fs` equivalent. These tools take no
//! subcommand verb — the read-vs-stdin / read-vs-write distinction lives
//! entirely in each rule's `guard`, so every rule uses an empty `subcommand`
//! (matches any invocation) and leans on the guard. The claude-code engine
//! aggregates this table via its `registries()` and appends the bypass hint to
//! the matched `message`.

use crate::hook::rule::HookRule;

/// Shared `grep`/`egrep`/`fgrep` redirect message (the three are byte-identical
/// reads, so they share one string and differ only by `tool`).
const GREP_MSG: &str = "Use `sak fs grep <pattern> <path>` instead of `grep`. \
     Flags: -i, -l, -c, -C N, --type, --glob, -U for multiline. \
     If you're spelunking a dump (a diff, JSON, issue text) for a fact, \
     query the source instead (br show <id>, sak json/git) rather than \
     grepping raw text; to drop a command's stderr noise use 2>/dev/null, \
     not a grep filter.";

pub const HOOK_RULES: &[HookRule] = &[
    HookRule {
        tool: "cat",
        subcommand: &[],
        guard: Some(cat_reads_a_file),
        message: "Use `sak fs read <file>` instead of `cat` for reading files. \
             Ranges: `-n 1-50` (lines), `-n -20` (last 20).",
    },
    HookRule {
        tool: "head",
        subcommand: &[],
        guard: Some(headtail_reads_a_file),
        message: "Use `sak fs head <file> [n]` instead of `head` (--bytes N, --no-line-numbers).",
    },
    HookRule {
        tool: "tail",
        subcommand: &[],
        guard: Some(headtail_reads_a_file),
        message: "Use `sak fs tail <file> [n]` instead of `tail` (--bytes N, --no-line-numbers).",
    },
    HookRule {
        tool: "grep",
        subcommand: &[],
        guard: Some(grep_reads_files),
        message: GREP_MSG,
    },
    HookRule {
        tool: "egrep",
        subcommand: &[],
        guard: Some(grep_reads_files),
        message: GREP_MSG,
    },
    HookRule {
        tool: "fgrep",
        subcommand: &[],
        guard: Some(grep_reads_files),
        message: GREP_MSG,
    },
    // `rg`/`ripgrep` are always reads (no stdin-vs-file ambiguity worth the
    // guard) — every invocation redirects. The `{tool}` is baked into the
    // message rather than interpolated.
    HookRule {
        tool: "rg",
        subcommand: &[],
        guard: None,
        message: "Use `sak fs grep <pattern> <path>` instead of `rg`.",
    },
    HookRule {
        tool: "ripgrep",
        subcommand: &[],
        guard: None,
        message: "Use `sak fs grep <pattern> <path>` instead of `ripgrep`.",
    },
    // `find` emits two messages, so it gets two rules: the metadata-search rule
    // is listed first (the engine takes the first match), and the broader
    // name-search rule catches the rest. Both decline when a write action flag
    // (`-exec`, `-delete`, ...) is present.
    HookRule {
        tool: "find",
        subcommand: &[],
        guard: Some(find_searches_by_metadata),
        message: "Use `sak fs find <path>` instead of `find` for metadata searches \
             (--size +1M, --mtime -7d, --type f|d|l, --name <glob>).",
    },
    HookRule {
        tool: "find",
        subcommand: &[],
        guard: Some(find_searches),
        message: "Use `sak fs glob '<pattern>'` instead of `find` for name searches \
             (or `sak fs find <path>` to filter by --size/--mtime/--type).",
    },
    HookRule {
        tool: "tree",
        subcommand: &[],
        guard: None,
        message: "Use `sak fs tree [path]` instead of `tree` \
             (--max-depth N, --dirs-only, --hidden).",
    },
    HookRule {
        tool: "stat",
        subcommand: &[],
        guard: Some(reads_a_path),
        message: "Use `sak fs stat <path...>` instead of `stat` (--format json).",
    },
    HookRule {
        tool: "wc",
        subcommand: &[],
        guard: Some(reads_a_path),
        message: "Use `sak fs wc [files...]` instead of `wc` (--lines/--words/--bytes).",
    },
];

/// `cat` reading a file: a non-heredoc invocation with at least one positional
/// (a bare `cat` or a `cat <<EOF` heredoc reads stdin — nothing to redirect).
fn cat_reads_a_file(args: &[String]) -> bool {
    if is_heredoc(args) {
        return false;
    }
    args.iter().any(|a| !a.starts_with('-'))
}

/// `head`/`tail` reading a file: like [`cat_reads_a_file`], but the positional
/// scan goes through [`headtail_file_args`] so the value consumed by a
/// separated `-c`/`-n`/`--bytes`/`--lines` flag (the `200` in `head -c 200`) is
/// not mistaken for a filename.
fn headtail_reads_a_file(args: &[String]) -> bool {
    if is_heredoc(args) {
        return false;
    }
    !headtail_file_args(args).is_empty()
}

/// A heredoc (`<<EOF`) feeds stdin, not a file — never a redirect target.
fn is_heredoc(args: &[String]) -> bool {
    args.iter().any(|a| a.contains("<<"))
}

/// File-argument positionals for `head`/`tail`, skipping the value consumed by a
/// *separated* `-c`/`-n`/`--bytes`/`--lines` flag (the `200` in `head -c 200`).
/// Combined / inline forms (`-c200`, `-20`, `--bytes=200`) are single
/// dash-prefixed tokens already dropped by the leading-`-` filter, so only the
/// separated case needs special handling.
fn headtail_file_args(args: &[String]) -> Vec<&str> {
    const VALUE_FLAGS: &[&str] = &["-c", "-n", "--bytes", "--lines"];
    let mut files = Vec::new();
    let mut skip_value = false;
    for a in args {
        if skip_value {
            skip_value = false;
            continue;
        }
        if VALUE_FLAGS.contains(&a.as_str()) {
            skip_value = true;
            continue;
        }
        if a.starts_with('-') {
            continue;
        }
        files.push(a.as_str());
    }
    files
}

/// `grep` reading files: a recursive flag (`-r`/`-R`/`--recursive`, including
/// bundled short flags like `-rn`) or two-or-more positionals (pattern + path).
/// A single positional (`grep foo`) or piped stdin reads stdin — allowed.
fn grep_reads_files(args: &[String]) -> bool {
    let recursive = args.iter().any(|a| {
        a == "-r"
            || a == "-R"
            || a == "--recursive"
            || (a.starts_with('-') && !a.starts_with("--") && a.contains('r'))
            || (a.starts_with('-') && !a.starts_with("--") && a.contains('R'))
    });
    let positionals = args.iter().filter(|a| !a.starts_with('-')).count();
    recursive || positionals >= 2
}

/// Write actions that take `find` out of read-only territory — never redirected.
fn find_is_write(args: &[String]) -> bool {
    const WRITE_FLAGS: &[&str] = &[
        "-exec", "-execdir", "-delete", "-ok", "-okdir", "-fprint", "-fprintf",
    ];
    args.iter().any(|a| WRITE_FLAGS.contains(&a.as_str()))
}

/// `find` searching by metadata predicate (`-size`/`-mtime`/`-type`/...) — maps
/// to `sak fs find`. Declines when a write action flag is present.
fn find_searches_by_metadata(args: &[String]) -> bool {
    if find_is_write(args) {
        return false;
    }
    args.iter().any(|a| {
        matches!(
            a.as_str(),
            "-size" | "-mtime" | "-mmin" | "-newer" | "-type"
        )
    })
}

/// Any non-write `find` (the name-search fallback after the metadata rule) —
/// maps to `sak fs glob`.
fn find_searches(args: &[String]) -> bool {
    !find_is_write(args)
}

/// `stat`/`wc` given a path: both are usage errors with no positional (bare
/// `stat`) or read stdin (piped `wc`), so they only redirect with a path arg.
fn reads_a_path(args: &[String]) -> bool {
    args.iter().any(|a| !a.starts_with('-'))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn a(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn cat_guard_distinguishes_file_from_stdin() {
        assert!(cat_reads_a_file(&a(&["/etc/passwd"])));
        assert!(cat_reads_a_file(&a(&["-n", "file.txt"])));
        // Bare cat / heredoc read stdin.
        assert!(!cat_reads_a_file(&a(&[])));
        assert!(!cat_reads_a_file(&a(&["<<EOF"])));
        assert!(!cat_reads_a_file(&a(&["<<-EOF"])));
    }

    #[test]
    fn headtail_guard_skips_value_flags() {
        // -c/-n/--bytes/--lines consume the next token, so a bare value is stdin.
        assert!(!headtail_reads_a_file(&a(&["-c", "200"])));
        assert!(!headtail_reads_a_file(&a(&["-n", "5"])));
        assert!(!headtail_reads_a_file(&a(&["--bytes", "200"])));
        assert!(!headtail_reads_a_file(&a(&["--lines", "5"])));
        // A real file after the consumed value still reads a file.
        assert!(headtail_reads_a_file(&a(&["-c", "200", "file.txt"])));
        assert!(headtail_reads_a_file(&a(&["-20", "file.txt"])));
        // Heredoc is stdin.
        assert!(!headtail_reads_a_file(&a(&["<<EOF"])));
    }

    #[test]
    fn grep_guard_recursive_or_multifile() {
        assert!(grep_reads_files(&a(&["-r", "foo", "."])));
        assert!(grep_reads_files(&a(&["-R", "foo", "."])));
        assert!(grep_reads_files(&a(&["--recursive", "foo", "."])));
        assert!(grep_reads_files(&a(&["-rn", "foo", "."])));
        assert!(grep_reads_files(&a(&["foo", "file.txt"])));
        // Single positional / stdin pipe is allowed.
        assert!(!grep_reads_files(&a(&["foo"])));
        assert!(!grep_reads_files(&a(&["-i", "foo"])));
    }

    #[test]
    fn find_guards_split_metadata_name_and_write() {
        // Metadata predicates take the metadata rule.
        assert!(find_searches_by_metadata(&a(&[".", "-size", "+1M"])));
        assert!(find_searches_by_metadata(&a(&[".", "-type", "f"])));
        // Name-only search misses the metadata rule but hits the broad rule.
        assert!(!find_searches_by_metadata(&a(&[".", "-name", "foo.rs"])));
        assert!(find_searches(&a(&[".", "-name", "foo.rs"])));
        // Write actions decline both.
        assert!(!find_searches_by_metadata(&a(&[
            ".", "-type", "f", "-delete"
        ])));
        assert!(!find_searches(&a(&[
            ".", "-name", "x", "-exec", "rm", "{}", ";"
        ])));
    }

    #[test]
    fn path_guard_requires_a_positional() {
        assert!(reads_a_path(&a(&["src/main.rs"])));
        assert!(reads_a_path(&a(&["-c", "%s", "file.txt"])));
        assert!(!reads_a_path(&a(&[])));
        assert!(!reads_a_path(&a(&["-l"])));
    }
}
