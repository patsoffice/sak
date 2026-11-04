//! Agent-hook redirect rules for the `fs` domain.
//!
//! Each row maps a read-only shell tool (`cat`, `head`, `tail`, `grep`, `rg`,
//! `find`, `tree`, `stat`, `wc`, `ls`, `cut`) to its `sak fs` equivalent.
//! These tools take no subcommand verb — the read-vs-stdin / read-vs-write
//! distinction lives entirely in each rule's `guard`, so every rule uses an
//! empty `subcommand` (matches any invocation) and leans on the guard. The
//! claude-code engine aggregates this table via its `registries()` and
//! appends the bypass hint to the matched `message`.
//!
//! `perl`, `sed`, and `awk` are a second kind of row: pure denies, not
//! redirects. They have no single `sak` equivalent (each is a full language),
//! so they block every invocation that runs a program, and their messages name
//! the closest sak command for the common shapes plus what to do when nothing
//! fits. A version/help query runs no program, so it passes. `node` and `ruby`
//! are narrower: they mostly run project code, so only inline programs block —
//! `-e`/`-p` one-liners and programs piped or heredoc'd in, the shapes models
//! swap in for a blocked `perl` — while `node build.js` passes. `python` gets
//! the same inline-program row (`-c`, or a program piped or heredoc'd in);
//! `python script.py` and `python -m <module>` pass here, and
//! `crate::json::hook` separately redirects `python -m json.tool`. Versioned
//! interpreter names (`python3.14`, `perl5.40.0`, `ruby3.3`) reach these rows
//! through the engine's `canonical_tool`.

use crate::hook::rule::HookRule;

/// Shared `grep`/`egrep`/`fgrep` redirect message (the three are byte-identical
/// reads, so they share one string and differ only by `tool`).
const GREP_MSG: &str = "Use `sak fs grep <pattern> <path>` instead of `grep` \
     (pass `-` as <path> to read stdin, e.g. `cmd | sak fs grep <pattern> -`). \
     Rust regex syntax: `a|b`, not `a\\|b`. \
     Flags: -i, -v, -l, -w, -e, -F, -o, -h, -C N, --type, --glob, -U for multiline. \
     Counting: -c (per file) or --total (one grand total) — don't pipe the \
     default output into a line counter. --no-heading gives bare \
     path:line:text rows. \
     If you're spelunking a dump (a diff, JSON, issue text) for a fact, \
     query the source instead (br show <id>, sak json/git) rather than \
     grepping raw text; to drop a command's stderr noise use 2>/dev/null, \
     not a grep filter.";

/// Shared `ag`/`ack` redirect message (same read shape as `grep`, same
/// `grep_reads` guard — only the binary name differs).
const AG_MSG: &str = "Use `sak fs grep <pattern> <path>` instead of `ag`/`ack` \
     (pass `-` as <path> to read stdin, e.g. `cmd | sak fs grep <pattern> -`). \
     Flags: -i, -v, -l, -w, -e, -F, -o, -h, -C N, --type, --glob, -U for multiline. \
     Counting: -c (per file) or --total (one grand total) — don't pipe the \
     default output into a line counter. --no-heading gives bare \
     path:line:text rows.";

/// Shared `find`/`gfind` metadata-search message.
const FIND_META_MSG: &str = "Use `sak fs find <path>` instead of `find`/`gfind` \
     for metadata searches (--size +1M, --mtime -7d, --type f|d|l, --name <glob>).";

/// Shared `find`/`gfind` name-search message.
const FIND_NAME_MSG: &str = "Use `sak fs glob '<pattern>'` instead of `find`/`gfind` \
     for name searches (or `sak fs find <path>` to filter by --size/--mtime/--type).";

/// Shared `ls`/`eza`/`exa` listing message.
const LS_MSG: &str = "Use `sak fs glob '<pattern>'` instead of `ls`/`eza`/`exa` \
     for name searches (or `sak fs tree [path]` to list a directory).";

/// Shared `awk`/`gawk`/`nawk`/`mawk` pure-deny message.
const AWK_MSG: &str = "Don't use `awk`/`gawk`/`nawk`/`mawk`; for field extraction use \
     `sak fs cut --whitespace -f <n>` (`awk '{print $2}'`; `-d <delim>` for \
     `awk -F`), for line \
     filtering use `sak fs grep <pattern>`. Anything else these tools do \
     has no sak equivalent — say what's missing and ask the user to run the command.";

/// Shared `sed`/`gsed` pure-deny message (`gsed` is brew GNU sed).
const SED_MSG: &str = "Don't use `sed`/`gsed`; to find lines use `sak fs grep <pattern>`, \
     to view a range use `sak fs read <file> -n <lo>-<hi>` \
     (`sed -n '10,20p'`). Anything else these tools do has no sak \
     equivalent — say what's missing and ask the user to run the command.";

pub const HOOK_RULES: &[HookRule] = &[
    HookRule {
        tool: "cat",
        subcommand: &[],
        guard: Some(cat_reads_a_file),
        message: "Use `sak fs read <file>` instead of `cat` for reading files. \
             Ranges: `-n 1-50` (lines), `-n -20` (last 20).",
    },
    // `bat` is `cat` with highlighting — same read shape, same guard (a file
    // positional reads a file; bare/`--help` reads stdin or nothing).
    HookRule {
        tool: "bat",
        subcommand: &[],
        guard: Some(cat_reads_a_file),
        message: "Use `sak fs read <file>` instead of `bat` for reading files. \
             Ranges: `-n 1-50` (lines), `-n -20` (last 20).",
    },
    HookRule {
        tool: "head",
        subcommand: &[],
        guard: Some(headtail_reads_a_file),
        message: "Use `sak fs head [-n N] <file> [n]` instead of `head` (--bytes N, --no-line-numbers).",
    },
    HookRule {
        tool: "tail",
        subcommand: &[],
        guard: Some(headtail_reads_a_file),
        message: "Use `sak fs tail [-n N] <file> [n]` instead of `tail` (--bytes N, --no-line-numbers, --follow [--for 30s]).",
    },
    HookRule {
        tool: "grep",
        subcommand: &[],
        guard: Some(grep_reads),
        message: GREP_MSG,
    },
    HookRule {
        tool: "egrep",
        subcommand: &[],
        guard: Some(grep_reads),
        message: GREP_MSG,
    },
    HookRule {
        tool: "fgrep",
        subcommand: &[],
        guard: Some(grep_reads),
        message: GREP_MSG,
    },
    // `rg` is always a read (no stdin-vs-file ambiguity worth the guard) —
    // every invocation redirects. The historical `ripgrep` spelling has no
    // upstream binary behind it (`rg` is the real name), so it gets no row.
    HookRule {
        tool: "rg",
        subcommand: &[],
        guard: None,
        message: "Use `sak fs grep <pattern> <path>` instead of `rg` \
            (pass `-` as <path> to read stdin).",
    },
    // `ag`/`ack` are grep with a different binary name — same redirect as
    // `grep` (same `grep_reads` guard: a positional pattern reads a file or
    // stdin; bare/`--help` has nothing to redirect).
    HookRule {
        tool: "ag",
        subcommand: &[],
        guard: Some(grep_reads),
        message: AG_MSG,
    },
    HookRule {
        tool: "ack",
        subcommand: &[],
        guard: Some(grep_reads),
        message: AG_MSG,
    },
    // `find` emits two messages, so it gets two rules: the metadata-search rule
    // is listed first (the engine takes the first match), and the broader
    // name-search rule catches the rest. Both decline when a write action flag
    // (`-exec`, `-delete`, ...) is present. `gfind` (brew findutils) is the
    // same binary under another name and shares both rules.
    HookRule {
        tool: "find",
        subcommand: &[],
        guard: Some(find_searches_by_metadata),
        message: FIND_META_MSG,
    },
    HookRule {
        tool: "find",
        subcommand: &[],
        guard: Some(find_searches),
        message: FIND_NAME_MSG,
    },
    HookRule {
        tool: "gfind",
        subcommand: &[],
        guard: Some(find_searches_by_metadata),
        message: FIND_META_MSG,
    },
    HookRule {
        tool: "gfind",
        subcommand: &[],
        guard: Some(find_searches),
        message: FIND_NAME_MSG,
    },
    // `fd`/`fdfind` are name searchers: `fd <pattern>` maps to `sak fs glob`.
    // Unlike `find`, the exec flags (`--exec`/`-x`, `--exec-batch`/`-X`) run
    // arbitrary commands, so those decline exactly like `find`'s write
    // actions — redirecting them would strand the model with a glob that
    // can't execute.
    HookRule {
        tool: "fd",
        subcommand: &[],
        guard: Some(fd_searches),
        message: "Use `sak fs glob '<pattern>'` instead of `fd`/`fdfind` \
             for name searches (add `--type f|d`, `--hidden`).",
    },
    HookRule {
        tool: "fdfind",
        subcommand: &[],
        guard: Some(fd_searches),
        message: "Use `sak fs glob '<pattern>'` instead of `fd`/`fdfind` \
             for name searches (add `--type f|d`, `--hidden`).",
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
        message: "Use `sak fs stat <path...>` instead of `stat` (--format json, --printf FMT).",
    },
    // `wc` is always a read (no write mode) and `sak fs wc` reads stdin when
    // files are omitted, so every invocation redirects — the piped
    // `cmd | wc -l` form maps to `cmd | sak fs wc --lines`, not just the
    // file-argument form. Hence `guard: None` rather than a path guard.
    HookRule {
        tool: "wc",
        subcommand: &[],
        guard: None,
        message: "Use `sak fs wc [files...]` instead of `wc` (--lines/--words/--bytes; \
            omit files to read stdin, e.g. `cmd | sak fs wc --lines`).",
    },
    // `ls` lists names, which is `sak fs glob`'s job (`tree` for a visual
    // listing). `eza`/`exa` are `ls` replacements. None has a write mode, so
    // like `rg`/`tree`/`wc` above, every invocation redirects.
    HookRule {
        tool: "ls",
        subcommand: &[],
        guard: None,
        message: LS_MSG,
    },
    HookRule {
        tool: "eza",
        subcommand: &[],
        guard: None,
        message: LS_MSG,
    },
    HookRule {
        tool: "exa",
        subcommand: &[],
        guard: None,
        message: LS_MSG,
    },
    // `cut` extracts delimiter-separated fields from files or stdin, exactly
    // what `sak fs cut` does (`-` reads stdin there too), so every invocation
    // redirects — the piped `cmd | cut -d, -f2` form maps to
    // `cmd | sak fs cut -d, -f2`.
    HookRule {
        tool: "cut",
        subcommand: &[],
        guard: None,
        message: "Use `sak fs cut -f <n>` instead of `cut` (tab-delimited by \
            default, like `cut`; `-d <delim>` for others; reads files or stdin \
            via `-`; --header, --filter, --regex-delim supported).",
    },
    // `perl`, `sed`, and `awk` are full languages with no single sak
    // equivalent, so these rows are pure denies: block every invocation that
    // runs a program and name the closest sak command for the shapes models
    // actually reach for. Anything beyond those shapes has no sak equivalent —
    // the message says to ask the user rather than guess a second tool.
    HookRule {
        tool: "awk",
        subcommand: &[],
        guard: Some(awk_runs_a_program),
        message: AWK_MSG,
    },
    // `gawk`/`nawk`/`mawk` are awk under other names — same guard, same deny.
    HookRule {
        tool: "gawk",
        subcommand: &[],
        guard: Some(awk_runs_a_program),
        message: AWK_MSG,
    },
    HookRule {
        tool: "nawk",
        subcommand: &[],
        guard: Some(awk_runs_a_program),
        message: AWK_MSG,
    },
    HookRule {
        tool: "mawk",
        subcommand: &[],
        guard: Some(awk_runs_a_program),
        message: AWK_MSG,
    },
    HookRule {
        tool: "sed",
        subcommand: &[],
        guard: Some(sed_runs_a_program),
        message: SED_MSG,
    },
    // `gsed` is brew GNU sed — same guard, same deny.
    HookRule {
        tool: "gsed",
        subcommand: &[],
        guard: Some(sed_runs_a_program),
        message: SED_MSG,
    },
    HookRule {
        tool: "perl",
        subcommand: &[],
        guard: Some(perl_runs_a_program),
        message: "Don't use `perl` for text or JSON processing; use \
            `sak fs grep <pattern>` to find lines, `sak json query <path>` \
            for JSON. Anything else `perl` does has no sak equivalent — say \
            what's missing and ask the user to run the command.",
    },
    // `node` and `ruby` mostly run project code, so only inline programs
    // block: the one-liners models swap in once `perl` is denied. `nodejs` is
    // the Debian/Ubuntu name for the same binary.
    HookRule {
        tool: "node",
        subcommand: &[],
        guard: Some(node_runs_inline_program),
        message: NODE_MSG,
    },
    HookRule {
        tool: "nodejs",
        subcommand: &[],
        guard: Some(node_runs_inline_program),
        message: NODE_MSG,
    },
    HookRule {
        tool: "ruby",
        subcommand: &[],
        guard: Some(ruby_runs_inline_program),
        message: "Don't run inline `ruby` programs (`-e`, or a program piped or \
            heredoc'd in) for text or JSON processing; use `sak fs grep <pattern>` \
            to find lines, `sak fs cut -d <delim> -f <n>` for fields, \
            `sak json query <path> <file>` for JSON. Anything else has no sak \
            equivalent — say what's missing and ask the user to run the command.",
    },
    // Inline `python` code is the main substitute once `perl`/`awk`/`sed` are
    // denied. Covers `python3`, `python3.14`, ... via `canonical_tool`.
    HookRule {
        tool: "python",
        subcommand: &[],
        guard: Some(python_runs_inline_program),
        message: "Don't run inline `python` code (`-c`, or a program piped or \
            heredoc'd in) to read or process files or output; for JSON use \
            `sak json query <path> <file>` (also `table`, `select`, `keys`), to \
            find lines `sak fs grep <pattern>`, to view part of a file \
            `sak fs read <file> -n <lo>-<hi>`, for fields `sak fs cut -d <delim> -f <n>`. \
            Anything else has no sak equivalent — say what's missing and ask the \
            user to run the command.",
    },
    // The other JavaScript runtimes take inline code too: `bun -e/-p`, `deno
    // eval`, and a program piped to `bun -` / `deno run -` / bare `deno`
    // (its REPL evaluates piped input). Scripts, tests, and tasks pass.
    HookRule {
        tool: "bun",
        subcommand: &[],
        guard: Some(bun_runs_inline_program),
        message: JS_RUNTIME_MSG,
    },
    HookRule {
        tool: "deno",
        subcommand: &[],
        guard: Some(deno_runs_inline_program),
        message: JS_RUNTIME_MSG,
    },
    // `php -r '<code>'`, the per-line `-B/-R/-E` modes, `-a`, and a program
    // piped to bare `php`. Covers `php8.3`, ... via `canonical_tool`.
    HookRule {
        tool: "php",
        subcommand: &[],
        guard: Some(php_runs_inline_program),
        message: "Don't run inline `php` code (`-r`, `-R`/`-B`/`-E`, `-a`, or a \
            program piped in) to read or process files or output; for JSON use \
            `sak json query <path> <file>`, to find lines `sak fs grep <pattern>`, \
            for fields `sak fs cut -d <delim> -f <n>`. Anything else has no sak \
            equivalent — say what's missing and ask the user to run the command.",
    },
];

/// Shared `bun`/`deno` message.
const JS_RUNTIME_MSG: &str = "Don't run inline JavaScript/TypeScript (`bun -e`/`-p`, \
     `deno eval`, or a program piped or heredoc'd in) for text or JSON \
     processing; use `sak json query <path> <file>` for JSON (e.g. `sak json \
     query version package.json`), `sak fs grep <pattern>` to find lines. \
     Anything else has no sak equivalent — say what's missing and ask the user \
     to run the command.";

/// Shared `node`/`nodejs` message (same binary under two names).
const NODE_MSG: &str = "Don't run inline `node` programs (`-e`/`-p`, or a program \
     piped or heredoc'd in) for text or JSON processing; use \
     `sak json query <path> <file>` for JSON (e.g. `sak json query version \
     package.json`), `sak fs grep <pattern>` to find lines. Anything else has \
     no sak equivalent — say what's missing and ask the user to run the command.";

/// An invocation made only of version/help/config flags prints information
/// and runs no program — a lookup, not a use. A bare invocation is *not* a
/// query: it reads a program from stdin (`echo … | perl`, `perl <<'EOF'`).
fn only_info_flags(args: &[String], flags: &[&str]) -> bool {
    !args.is_empty() && args.iter().all(|a| flags.contains(&a.as_str()))
}

/// `perl` running a program: anything but `-v`/`-V`/`-V:name`/`-h` and their
/// long spellings.
fn perl_runs_a_program(args: &[String]) -> bool {
    const INFO: &[&str] = &["-v", "--version", "-V", "-h", "--help"];
    !(!args.is_empty()
        && args
            .iter()
            .all(|a| INFO.contains(&a.as_str()) || a.starts_with("-V:")))
}

/// `sed` running a script: anything but GNU's `--version`/`--help`.
fn sed_runs_a_program(args: &[String]) -> bool {
    !only_info_flags(args, &["--version", "--help"])
}

/// `awk` running a program: anything but a version/help query in gawk
/// (`--version`/`-V`, `--help`/`-h`) or mawk (`-W version`, `-Wversion`,
/// `-Wv`, `-Wusage`) spelling.
fn awk_runs_a_program(args: &[String]) -> bool {
    const INFO: &[&str] = &[
        "--version",
        "-V",
        "--help",
        "-h",
        "-Wversion",
        "-Wv",
        "-Wusage",
    ];
    let mawk_pair = matches!(args, [w, v] if w == "-W" && (v == "version" || v == "usage"));
    !(mawk_pair || only_info_flags(args, INFO))
}

/// How one interpreter spells its options, for [`runs_inline_program`].
struct InlineSyntax {
    /// Short flags whose program is the next argument (`-e`, node's `-p`).
    eval_short: &'static str,
    /// Long flags that take an inline program (`--eval`, `--print`).
    eval_long: &'static [&'static str],
    /// Short flags with a required value: attached (`-rjson`) or, at the end
    /// of a cluster, the next argument (`-r json`).
    value_short: &'static str,
    /// Short flags with an optional value that can only be attached
    /// (`-i.bak`, `-W0`): the rest of the cluster is the value.
    attached_short: &'static str,
    /// Long flags whose value may be the next argument (`--require x`).
    value_long: &'static [&'static str],
    /// Long flags that run project code with no script operand
    /// (`node --test`, `node --run build`).
    runs_project: &'static [&'static str],
    /// Short flags that run project code named by their value
    /// (`python -m pytest`).
    project_short: &'static str,
    /// Version/help flags: an invocation of only these runs nothing.
    info: &'static [&'static str],
    /// Whether an invocation with no script reads its program from stdin
    /// (`echo … | node`). `bun` prints help instead.
    bare_reads_stdin: bool,
}

const NODE_SYNTAX: InlineSyntax = InlineSyntax {
    eval_short: "ep",
    eval_long: &["eval", "print"],
    value_short: "rC",
    attached_short: "",
    value_long: &[
        "require",
        "import",
        "loader",
        "experimental-loader",
        "conditions",
        "input-type",
        "title",
    ],
    runs_project: &["test", "run"],
    project_short: "",
    info: &["-v", "--version", "-h", "--help", "--v8-options"],
    bare_reads_stdin: true,
};

const RUBY_SYNTAX: InlineSyntax = InlineSyntax {
    eval_short: "e",
    eval_long: &[],
    value_short: "rICE",
    attached_short: "0FiKTWx",
    value_long: &[
        "enable",
        "disable",
        "encoding",
        "external-encoding",
        "internal-encoding",
        "dump",
        "backtrace-limit",
    ],
    runs_project: &[],
    project_short: "",
    info: &["-v", "--version", "-h", "--help", "--copyright"],
    bare_reads_stdin: true,
};

const PYTHON_SYNTAX: InlineSyntax = InlineSyntax {
    eval_short: "c",
    eval_long: &[],
    value_short: "WX",
    attached_short: "",
    value_long: &["check-hash-based-pycs"],
    runs_project: &[],
    project_short: "m",
    info: &[
        "-V",
        "-VV",
        "--version",
        "-h",
        "-?",
        "--help",
        "--help-env",
        "--help-xoptions",
        "--help-all",
    ],
    bare_reads_stdin: true,
};

/// `bun`'s top-level options, also accepted after `bun run`.
const BUN_SYNTAX: InlineSyntax = InlineSyntax {
    eval_short: "ep",
    eval_long: &["eval", "print"],
    value_short: "r",
    attached_short: "",
    value_long: &["preload", "require", "import", "cwd", "env-file", "config"],
    runs_project: &[],
    project_short: "",
    info: &["-v", "--version", "-h", "--help", "--revision"],
    bare_reads_stdin: false,
};

/// `php`'s CLI options (`php -h`). `-e` is the debugger flag, not eval; `-f`,
/// `-F`, and `-S` run a file or the built-in server; `-l` only lints.
const PHP_SYNTAX: InlineSyntax = InlineSyntax {
    eval_short: "rRBEa",
    eval_long: &[
        "run",
        "process-code",
        "process-begin",
        "process-end",
        "interactive",
    ],
    value_short: "cdtz",
    attached_short: "",
    value_long: &[
        "php-ini",
        "define",
        "docroot",
        "zend-extension",
        "rf",
        "rc",
        "re",
        "rz",
        "ri",
    ],
    runs_project: &["file", "process-file", "server", "syntax-check"],
    project_short: "fFSl",
    info: &[
        "-v",
        "--version",
        "-h",
        "--help",
        "-i",
        "--info",
        "-m",
        "--modules",
        "--ini",
        "--ini=diff",
    ],
    bare_reads_stdin: true,
};

fn node_runs_inline_program(args: &[String]) -> bool {
    runs_inline_program(args, &NODE_SYNTAX)
}

/// `bun` running inline code: its eval/print flags or `-`, either at the top
/// level or after `bun run`. Other subcommands (`test`, `x`, `install`,
/// `build`) and script files pass, as does a bare `bun` (it prints help).
fn bun_runs_inline_program(args: &[String]) -> bool {
    let args = match args.first().map(String::as_str) {
        Some("run") => &args[1..],
        _ => args,
    };
    runs_inline_program(args, &BUN_SYNTAX)
}

/// `deno` running inline code, decided by its first positional: `eval`,
/// `repl` (it evaluates piped input, and `--eval` code), `-`, `run -`, or none
/// at all (bare `deno` is the REPL). Scripts, `run <file>`, and the other
/// subcommands (`task`, `test`, `x`, `fmt`, ...) pass.
fn deno_runs_inline_program(args: &[String]) -> bool {
    if only_info_flags(args, &["-V", "--version", "-h", "--help"]) {
        return false;
    }
    let mut pos = args
        .iter()
        .map(String::as_str)
        .filter(|a| !a.starts_with('-') || *a == "-");
    match pos.next() {
        None | Some("-" | "eval" | "repl") => true,
        Some("run") => {
            let rest: Vec<&str> = pos.collect();
            rest.is_empty() || rest.contains(&"-")
        }
        Some(_) => false,
    }
}

fn php_runs_inline_program(args: &[String]) -> bool {
    runs_inline_program(args, &PHP_SYNTAX)
}

fn ruby_runs_inline_program(args: &[String]) -> bool {
    runs_inline_program(args, &RUBY_SYNTAX)
}

fn python_runs_inline_program(args: &[String]) -> bool {
    runs_inline_program(args, &PYTHON_SYNTAX)
}

/// An interpreter running an inline program: one given on the command line
/// (`-e`, `--eval`) or read from stdin (`-` as the script, or no script at all
/// — a piped or heredoc'd program). The scan stops at the first positional:
/// that's the script file, and everything after it is the script's own
/// arguments (`node build.js -e x` runs `build.js`). Unrecognized value flags
/// err toward allowing: their value reads as the script.
fn runs_inline_program(args: &[String], syn: &InlineSyntax) -> bool {
    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        if a == "-" {
            return true;
        }
        if a == "--" {
            return args.get(i + 1).is_none_or(|s| s == "-");
        }
        if let Some(long) = a.strip_prefix("--") {
            let (name, inline_value) = match long.split_once('=') {
                Some((n, _)) => (n, true),
                None => (long, false),
            };
            if syn.eval_long.contains(&name) {
                return true;
            }
            if syn.runs_project.contains(&name) {
                return false;
            }
            if syn.value_long.contains(&name) && !inline_value {
                i += 1;
            }
            i += 1;
            continue;
        }
        if let Some(cluster) = a.strip_prefix('-') {
            for (pos, c) in cluster.char_indices() {
                if syn.eval_short.contains(c) {
                    return true;
                }
                if syn.project_short.contains(c) {
                    return false;
                }
                if syn.attached_short.contains(c) {
                    break;
                }
                if syn.value_short.contains(c) {
                    if pos + c.len_utf8() == cluster.len() {
                        i += 1;
                    }
                    break;
                }
            }
            i += 1;
            continue;
        }
        return false;
    }
    syn.bare_reads_stdin && !only_info_flags(args, syn.info)
}

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

/// `grep` is always a read — it has no write mode. It reads a file when given a
/// path positional and reads stdin otherwise (`cmd | grep foo`, `grep foo`),
/// and `sak fs grep` handles both (`-` for stdin), so any invocation carrying a
/// pattern positional redirects. The lone non-reads — `grep --help`/`--version`
/// and a bare `grep` — have no positional, so they pass through.
fn grep_reads(args: &[String]) -> bool {
    args.iter().any(|a| !a.starts_with('-'))
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

/// `fd`/`fdfind` doing name search: anything but the exec flags (`-x`/`--exec,
/// `-X`/`--exec-batch`), which run arbitrary commands sak can't run. Those
/// decline exactly like `find`'s write actions.
fn fd_searches(args: &[String]) -> bool {
    const EXEC_FLAGS: &[&str] = &["-x", "--exec", "-X", "--exec-batch"];
    !args.iter().any(|a| EXEC_FLAGS.contains(&a.as_str()))
}

/// `stat` given a path: bare `stat` is a usage error and `stat` has no stdin
/// mode, so it only redirects with a path positional. (`wc`, which *does* read
/// stdin, no longer shares this guard — it redirects unconditionally.)
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
    fn grep_guard_fires_on_any_pattern() {
        // File reads (the original cases) still fire.
        assert!(grep_reads(&a(&["-r", "foo", "."])));
        assert!(grep_reads(&a(&["-rn", "foo", "."])));
        assert!(grep_reads(&a(&["foo", "file.txt"])));
        // Stdin reads now fire too — sak handles them via `-`.
        assert!(grep_reads(&a(&["foo"])));
        assert!(grep_reads(&a(&["-i", "foo"])));
        // Inverted reads fire too — sak handles them via `-v`.
        assert!(grep_reads(&a(&["-v", "foo", "file.txt"])));
        assert!(grep_reads(&a(&["-v", "foo"])));
        // No positional (bare grep / `--help`) has nothing to redirect.
        assert!(!grep_reads(&a(&[])));
        assert!(!grep_reads(&a(&["--help"])));
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
    fn headtail_messages_name_dash_n() {
        let msgs: Vec<&&str> = HOOK_RULES
            .iter()
            .filter(|r| r.tool == "head" || r.tool == "tail")
            .map(|r| &r.message)
            .collect();
        assert_eq!(msgs.len(), 2);
        for m in msgs {
            assert!(m.contains("-n"), "{m}");
        }
    }

    #[test]
    fn pure_deny_guards_pass_info_queries_only() {
        // Version/help/config queries run no program.
        assert!(!perl_runs_a_program(&a(&["-v"])));
        assert!(!perl_runs_a_program(&a(&["-V:version"])));
        assert!(!sed_runs_a_program(&a(&["--version"])));
        assert!(!awk_runs_a_program(&a(&["--version"])));
        assert!(!awk_runs_a_program(&a(&["-W", "version"])));
        // Bare means a stdin program; a query flag next to a program still runs it.
        assert!(perl_runs_a_program(&a(&[])));
        assert!(perl_runs_a_program(&a(&["-v", "script.pl"])));
        assert!(sed_runs_a_program(&a(&["-n", "10,20p", "f"])));
        assert!(awk_runs_a_program(&a(&["-W", "posix", "{print}"])));
    }

    #[test]
    fn node_guard_blocks_inline_programs_only() {
        // Inline programs: eval/print flags (clustered too), `-`, bare.
        assert!(node_runs_inline_program(&a(&["-e", "1"])));
        assert!(node_runs_inline_program(&a(&["-pe", "1"])));
        assert!(node_runs_inline_program(&a(&["--print=1"])));
        assert!(node_runs_inline_program(&a(&[
            "--input-type",
            "module",
            "-"
        ])));
        assert!(node_runs_inline_program(&a(&[])));
        // Script files, project runners, and queries pass.
        assert!(!node_runs_inline_program(&a(&["build.js"])));
        assert!(!node_runs_inline_program(&a(&[
            "-r",
            "dotenv/config",
            "server.js"
        ])));
        assert!(!node_runs_inline_program(&a(&["build.js", "-e", "x"])));
        assert!(!node_runs_inline_program(&a(&["--test"])));
        assert!(!node_runs_inline_program(&a(&["--version"])));
    }

    #[test]
    fn ruby_guard_blocks_inline_programs_only() {
        assert!(ruby_runs_inline_program(&a(&["-e", "p 1"])));
        assert!(ruby_runs_inline_program(&a(&["-ne", "print"])));
        assert!(ruby_runs_inline_program(&a(&["-rjson", "-e", "p 1"])));
        assert!(ruby_runs_inline_program(&a(&["-i.bak", "-pe", "x"])));
        assert!(ruby_runs_inline_program(&a(&["-"])));
        assert!(ruby_runs_inline_program(&a(&[])));
        // An `e` inside a flag's value is not `-e`: `-rset` requires `set`.
        assert!(!ruby_runs_inline_program(&a(&["-rset", "script.rb"])));
        assert!(!ruby_runs_inline_program(&a(&["-I", "lib", "script.rb"])));
        assert!(!ruby_runs_inline_program(&a(&[
            "-n",
            "filter.rb",
            "data.txt"
        ])));
        assert!(!ruby_runs_inline_program(&a(&["-v"])));
    }

    #[test]
    fn fd_guard_declines_exec_only() {
        assert!(fd_searches(&a(&["foo"])));
        assert!(fd_searches(&a(&["-t", "f", "foo", "."])));
        assert!(fd_searches(&a(&["--hidden", "foo"])));
        // Exec flags run arbitrary commands — no sak equivalent, pass through.
        assert!(!fd_searches(&a(&["foo", "-x", "rm", "{}"])));
        assert!(!fd_searches(&a(&["foo", "--exec", "rm"])));
        assert!(!fd_searches(&a(&["foo", "-X", "rm"])));
        assert!(!fd_searches(&a(&["foo", "--exec-batch", "rm"])));
    }

    #[test]
    fn python_guard_blocks_inline_code_only() {
        assert!(python_runs_inline_program(&a(&["-c", "print(1)"])));
        assert!(python_runs_inline_program(&a(&["-Ic", "print(1)"])));
        assert!(python_runs_inline_program(&a(&["-W", "ignore", "-c", "x"])));
        assert!(python_runs_inline_program(&a(&["-"])));
        assert!(python_runs_inline_program(&a(&[])));
        // Script files, modules, and queries pass; a script's own `-c` is its arg.
        assert!(!python_runs_inline_program(&a(&["script.py"])));
        assert!(!python_runs_inline_program(&a(&[
            "-I",
            "script.py",
            "-c",
            "x"
        ])));
        assert!(!python_runs_inline_program(&a(&["-m", "pytest", "-q"])));
        assert!(!python_runs_inline_program(&a(&["-Im", "http.server"])));
        assert!(!python_runs_inline_program(&a(&["--version"])));
    }

    #[test]
    fn bun_guard_blocks_eval_and_stdin_only() {
        assert!(bun_runs_inline_program(&a(&["-e", "1"])));
        assert!(bun_runs_inline_program(&a(&["-pe", "1"])));
        assert!(bun_runs_inline_program(&a(&["--print=1"])));
        assert!(bun_runs_inline_program(&a(&["run", "-e", "1"])));
        assert!(bun_runs_inline_program(&a(&["-"])));
        assert!(bun_runs_inline_program(&a(&["run", "-"])));
        // Bare `bun` prints help; scripts and subcommands run project code.
        assert!(!bun_runs_inline_program(&a(&[])));
        assert!(!bun_runs_inline_program(&a(&["run", "build.ts"])));
        assert!(!bun_runs_inline_program(&a(&["test"])));
        assert!(!bun_runs_inline_program(&a(&["x", "vite"])));
        assert!(!bun_runs_inline_program(&a(&["--version"])));
    }

    #[test]
    fn deno_guard_follows_the_first_positional() {
        assert!(deno_runs_inline_program(&a(&["eval", "console.log(1)"])));
        assert!(deno_runs_inline_program(&a(&["eval", "-p", "1"])));
        assert!(deno_runs_inline_program(&a(&["repl", "--eval", "x"])));
        assert!(deno_runs_inline_program(&a(&["run", "-"])));
        assert!(deno_runs_inline_program(&a(&["-"])));
        assert!(deno_runs_inline_program(&a(&[])));
        assert!(deno_runs_inline_program(&a(&["-A"])));
        assert!(!deno_runs_inline_program(&a(&["run", "-A", "main.ts"])));
        assert!(!deno_runs_inline_program(&a(&["main.ts"])));
        assert!(!deno_runs_inline_program(&a(&["task", "build"])));
        assert!(!deno_runs_inline_program(&a(&["test"])));
        assert!(!deno_runs_inline_program(&a(&["--version"])));
    }

    #[test]
    fn php_guard_blocks_inline_code_only() {
        assert!(php_runs_inline_program(&a(&["-r", "echo 1;"])));
        assert!(php_runs_inline_program(&a(&["--run", "echo 1;"])));
        assert!(php_runs_inline_program(&a(&["-R", "echo $argn;"])));
        assert!(php_runs_inline_program(&a(&["-B", "x", "-F", "f.php"])));
        assert!(php_runs_inline_program(&a(&["-a"])));
        assert!(php_runs_inline_program(&a(&[
            "-d",
            "memory_limit=1G",
            "-r",
            "x"
        ])));
        assert!(php_runs_inline_program(&a(&[])));
        // `-e` is the debugger flag; files, the server, lint, and info pass.
        assert!(!php_runs_inline_program(&a(&["-e", "script.php"])));
        assert!(!php_runs_inline_program(&a(&["script.php"])));
        assert!(!php_runs_inline_program(&a(&["-f", "script.php"])));
        assert!(!php_runs_inline_program(&a(&["-S", "localhost:8000"])));
        assert!(!php_runs_inline_program(&a(&["-l", "script.php"])));
        assert!(!php_runs_inline_program(&a(&["-v"])));
        assert!(!php_runs_inline_program(&a(&["-m"])));
    }

    #[test]
    fn path_guard_requires_a_positional() {
        assert!(reads_a_path(&a(&["src/main.rs"])));
        assert!(reads_a_path(&a(&["-c", "%s", "file.txt"])));
        assert!(!reads_a_path(&a(&[])));
        assert!(!reads_a_path(&a(&["-l"])));
    }
}
