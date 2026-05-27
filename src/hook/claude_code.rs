//! Claude Code pre-tool-use hook.
//!
//! Reads the Claude Code `PreToolUse` JSON payload from stdin and decides
//! whether the about-to-run Bash command should be redirected to a `sak`
//! equivalent. Exits 0 to allow, exits 2 with a stderr message to block.
//! Set `SAK_HOOK_BYPASS=1` in the Bash command's environment to disable the
//! hook for one call.
//!
//! Configure in `~/.claude/settings.json`:
//!
//! ```json
//! {
//!   "hooks": {
//!     "PreToolUse": [
//!       {
//!         "matcher": "Bash",
//!         "hooks": [{ "type": "command", "command": "sak hook claude-code" }]
//!       }
//!     ]
//!   }
//! }
//! ```

use std::io::{self, Read};
use std::process::ExitCode;

use anyhow::Result;
use clap::Args;
use serde::Deserialize;

#[derive(Args)]
#[command(
    about = "Pre-tool-use hook for Claude Code",
    long_about = "Pre-tool-use hook for Claude Code (claude.com/claude-code).\n\n\
        Reads the harness's PreToolUse JSON payload from stdin. When the \
        about-to-run Bash command has a read-only `sak` equivalent, exit 2 \
        with a stderr message naming the replacement (Claude Code surfaces \
        that message back to the model). All other commands pass through with \
        exit 0.\n\n\
        Set SAK_HOOK_BYPASS=1 in the Bash command's environment to disable the \
        hook for one call.",
    after_help = "\
Configure in ~/.claude/settings.json:

  {
    \"hooks\": {
      \"PreToolUse\": [
        {
          \"matcher\": \"Bash\",
          \"hooks\": [{ \"type\": \"command\", \"command\": \"sak hook claude-code\" }]
        }
      ]
    }
  }

Examples:
  sak hook claude-code                   Read JSON payload from stdin (normal use)
  sak hook claude-code --check 'git log' Test a command directly (no stdin)
  SAK_HOOK_BYPASS=1 git status           One-shot escape hatch"
)]
pub struct ClaudeCodeArgs {
    /// Classify this command string directly instead of reading stdin.
    /// Intended for debugging the rule set from the shell.
    #[arg(long, value_name = "COMMAND")]
    pub check: Option<String>,
}

#[derive(Deserialize)]
struct HookPayload {
    #[serde(default)]
    tool_name: String,
    #[serde(default)]
    tool_input: ToolInput,
}

#[derive(Default, Deserialize)]
struct ToolInput {
    #[serde(default)]
    command: String,
}

pub fn run(args: &ClaudeCodeArgs) -> Result<ExitCode> {
    if std::env::var("SAK_HOOK_BYPASS").as_deref() == Ok("1") {
        return Ok(ExitCode::SUCCESS);
    }

    let command = match &args.check {
        Some(cmd) => cmd.clone(),
        None => {
            let mut buf = String::new();
            io::stdin().read_to_string(&mut buf)?;
            // Empty stdin → nothing to check.
            if buf.trim().is_empty() {
                return Ok(ExitCode::SUCCESS);
            }
            let payload: HookPayload = match serde_json::from_str(&buf) {
                Ok(p) => p,
                // Malformed JSON shouldn't block real work — fail open.
                Err(_) => return Ok(ExitCode::SUCCESS),
            };
            // Only intercept Bash tool calls.
            if payload.tool_name != "Bash" {
                return Ok(ExitCode::SUCCESS);
            }
            payload.tool_input.command
        }
    };

    if command.trim().is_empty() {
        return Ok(ExitCode::SUCCESS);
    }

    if let Some(msg) = classify(&command) {
        eprintln!("{}", msg);
        return Ok(ExitCode::from(2));
    }

    Ok(ExitCode::SUCCESS)
}

const BYPASS_HINT: &str = " Set SAK_HOOK_BYPASS=1 to override.";

/// Classify a full command string. Splits on shell separators (|, ||, &&, ;, &)
/// while respecting quotes, then evaluates each piece. First block wins.
pub(crate) fn classify(command: &str) -> Option<String> {
    for part in split_pipeline(command) {
        let tokens = tokenize(&part);
        let tokens = strip_env_assignments(tokens);
        if tokens.is_empty() {
            continue;
        }
        if let Some(msg) = check(&tokens) {
            return Some(msg);
        }
    }
    None
}

/// Split on `|`, `||`, `&&`, `;`, `&` outside of single/double quotes and
/// outside of backslash escapes.
fn split_pipeline(cmd: &str) -> Vec<String> {
    let bytes = cmd.as_bytes();
    let mut parts = Vec::new();
    let mut buf = String::new();
    let mut i = 0;
    let n = bytes.len();
    let mut in_single = false;
    let mut in_double = false;

    while i < n {
        let c = bytes[i] as char;
        if in_single {
            buf.push(c);
            if c == '\'' {
                in_single = false;
            }
            i += 1;
            continue;
        }
        if in_double {
            if c == '\\' && i + 1 < n {
                buf.push(c);
                buf.push(bytes[i + 1] as char);
                i += 2;
                continue;
            }
            buf.push(c);
            if c == '"' {
                in_double = false;
            }
            i += 1;
            continue;
        }
        if c == '\'' {
            in_single = true;
            buf.push(c);
            i += 1;
            continue;
        }
        if c == '"' {
            in_double = true;
            buf.push(c);
            i += 1;
            continue;
        }
        if c == '\\' && i + 1 < n {
            buf.push(c);
            buf.push(bytes[i + 1] as char);
            i += 2;
            continue;
        }
        if c == '|' {
            push_trimmed(&mut parts, &mut buf);
            i += if i + 1 < n && bytes[i + 1] == b'|' {
                2
            } else {
                1
            };
            continue;
        }
        if c == '&' {
            push_trimmed(&mut parts, &mut buf);
            i += if i + 1 < n && bytes[i + 1] == b'&' {
                2
            } else {
                1
            };
            continue;
        }
        if c == ';' {
            push_trimmed(&mut parts, &mut buf);
            i += 1;
            continue;
        }
        buf.push(c);
        i += 1;
    }
    push_trimmed(&mut parts, &mut buf);
    parts.retain(|p| !p.is_empty());
    parts
}

fn push_trimmed(parts: &mut Vec<String>, buf: &mut String) {
    let trimmed = buf.trim().to_string();
    if !trimmed.is_empty() {
        parts.push(trimmed);
    }
    buf.clear();
}

/// Tokenize a single command part using rough shell semantics: split on
/// whitespace outside of quotes, strip the outer quote chars, honor backslash
/// escapes. Unclosed quotes are silently tolerated (we'd rather under-block
/// than panic on weird input).
fn tokenize(part: &str) -> Vec<String> {
    let bytes = part.as_bytes();
    let mut tokens = Vec::new();
    let mut cur = String::new();
    let mut in_token = false;
    let mut in_single = false;
    let mut in_double = false;
    let mut i = 0;
    let n = bytes.len();

    while i < n {
        let c = bytes[i] as char;
        if in_single {
            if c == '\'' {
                in_single = false;
            } else {
                cur.push(c);
            }
            i += 1;
            continue;
        }
        if in_double {
            if c == '\\' && i + 1 < n {
                cur.push(bytes[i + 1] as char);
                i += 2;
                continue;
            }
            if c == '"' {
                in_double = false;
            } else {
                cur.push(c);
            }
            i += 1;
            continue;
        }
        if c.is_whitespace() {
            if in_token {
                tokens.push(std::mem::take(&mut cur));
                in_token = false;
            }
            i += 1;
            continue;
        }
        if c == '\'' {
            in_single = true;
            in_token = true;
            i += 1;
            continue;
        }
        if c == '"' {
            in_double = true;
            in_token = true;
            i += 1;
            continue;
        }
        if c == '\\' && i + 1 < n {
            cur.push(bytes[i + 1] as char);
            in_token = true;
            i += 2;
            continue;
        }
        cur.push(c);
        in_token = true;
        i += 1;
    }
    if in_token {
        tokens.push(cur);
    }
    tokens
}

/// Drop leading `FOO=bar BAZ=qux` env-var assignments — they prefix the real
/// command name in shell syntax.
fn strip_env_assignments(tokens: Vec<String>) -> Vec<String> {
    let mut i = 0;
    while i < tokens.len() {
        let t = &tokens[i];
        let Some(eq) = t.find('=') else { break };
        if eq == 0 {
            break;
        }
        let name = &t[..eq];
        let first = name.as_bytes()[0];
        if !(first.is_ascii_alphabetic() || first == b'_') {
            break;
        }
        if !name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') {
            break;
        }
        i += 1;
    }
    tokens[i..].to_vec()
}

/// Args that don't start with `-`.
fn positionals(args: &[String]) -> Vec<&str> {
    args.iter()
        .filter(|a| !a.starts_with('-'))
        .map(|s| s.as_str())
        .collect()
}

fn basename(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

fn block(msg: &str) -> Option<String> {
    Some(format!("{}{}", msg, BYPASS_HINT))
}

fn check(tokens: &[String]) -> Option<String> {
    let cmd_base = basename(&tokens[0]);
    let args: &[String] = &tokens[1..];
    let pos = positionals(args);

    match cmd_base {
        "cat" | "head" | "tail" => check_cat(cmd_base, args, &pos),
        "grep" | "egrep" | "fgrep" => check_grep(cmd_base, args, &pos),
        "rg" | "ripgrep" => block(&format!(
            "Use `sak fs grep <pattern> <path>` instead of `{cmd_base}`."
        )),
        "find" => check_find(args),
        "tree" => check_tree(&pos),
        "stat" => check_stat(&pos),
        "wc" => check_wc(&pos),
        "jq" => check_jq(&pos),
        "yq" | "tomlq" => check_yq(cmd_base, &pos),
        "plistutil" => block("Use `sak config query/keys/flatten <file>` instead of `plistutil`."),
        "openssl" => check_openssl(args),
        "sha256sum" | "sha1sum" | "md5sum" | "shasum" | "b3sum" => check_sum(cmd_base),
        "git" => check_git(args),
        "kubectl" => check_kubectl(&pos),
        "talosctl" => check_talosctl(&pos),
        "docker" => check_docker(&pos),
        "lxc" | "incus" => check_lxc(cmd_base, &pos),
        "gh" => check_gh(args, &pos),
        "helm" => check_helm(&pos),
        "nix" => check_nix(args, &pos),
        "nix-store" => check_nix_store(args),
        "sqlite3" => check_sqlite(args),
        "sysctl" => check_sysctl(args),
        _ => None,
    }
}

fn check_cat(base: &str, args: &[String], pos: &[&str]) -> Option<String> {
    // Heredoc (`<<EOF`) means stdin, not a file — allow.
    if args.iter().any(|a| a.contains("<<")) {
        return None;
    }
    // `head`/`tail` take a value after `-c`/`-n`/`--bytes`/`--lines`. The shared
    // `positionals()` helper doesn't know that, so it would mistake the value in
    // `head -c 200` (a stdin read) for a filename and wrongly redirect it.
    // Recompute file args for those two bases, skipping the consumed value.
    let file_args: Vec<&str> = match base {
        "head" | "tail" => headtail_file_args(args),
        _ => pos.to_vec(),
    };
    if file_args.is_empty() {
        return None;
    }
    // `head`/`tail` have dedicated, more ergonomic sak commands; `cat` maps to
    // `read`. `tail -f` (follow) has no read-only sak equivalent, but it's still
    // a read, so steer it to `sak fs tail` like the rest.
    match base {
        "head" => {
            block("Use `sak fs head <file> [n]` instead of `head` (--bytes N, --no-line-numbers).")
        }
        "tail" => {
            block("Use `sak fs tail <file> [n]` instead of `tail` (--bytes N, --no-line-numbers).")
        }
        _ => block(
            "Use `sak fs read <file>` instead of `cat` for reading files. \
             Ranges: `-n 1-50` (lines), `-n -20` (last 20).",
        ),
    }
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

/// `tree` is always a read — redirect every invocation to `sak fs tree`.
fn check_tree(_pos: &[&str]) -> Option<String> {
    block(
        "Use `sak fs tree [path]` instead of `tree` \
         (--max-depth N, --dirs-only, --hidden).",
    )
}

/// `stat` is always a read — redirect when given a path (bare `stat` is a usage
/// error, nothing to redirect).
fn check_stat(pos: &[&str]) -> Option<String> {
    if pos.is_empty() {
        return None;
    }
    block("Use `sak fs stat <path...>` instead of `stat` (--format json).")
}

/// `wc` reading files maps to `sak fs wc`; a bare `wc` (or piped stdin) reads
/// standard input and has nothing to redirect.
fn check_wc(pos: &[&str]) -> Option<String> {
    if pos.is_empty() {
        return None;
    }
    block("Use `sak fs wc [files...]` instead of `wc` (--lines/--words/--bytes).")
}

fn check_grep(_base: &str, args: &[String], pos: &[&str]) -> Option<String> {
    let recursive = args.iter().any(|a| {
        a == "-r"
            || a == "-R"
            || a == "--recursive"
            || (a.starts_with('-') && !a.starts_with("--") && a.contains('r'))
            || (a.starts_with('-') && !a.starts_with("--") && a.contains('R'))
    });
    if recursive || pos.len() >= 2 {
        return block(
            "Use `sak fs grep <pattern> <path>` instead of `grep`. \
             Flags: -i, -l, -c, -C N, --type, --glob, -U for multiline. \
             If you're spelunking a dump (a diff, JSON, issue text) for a fact, \
             query the source instead (br show <id>, sak json/git) rather than \
             grepping raw text; to drop a command's stderr noise use 2>/dev/null, \
             not a grep filter.",
        );
    }
    None
}

fn check_find(args: &[String]) -> Option<String> {
    let write_flags = [
        "-exec", "-execdir", "-delete", "-ok", "-okdir", "-fprint", "-fprintf",
    ];
    if args.iter().any(|a| write_flags.contains(&a.as_str())) {
        return None;
    }
    // A `find` with metadata predicates (-size/-mtime/-type) maps to `sak fs
    // find`; a name-only search maps to `sak fs glob`. Mention both.
    let by_metadata = args.iter().any(|a| {
        matches!(
            a.as_str(),
            "-size" | "-mtime" | "-mmin" | "-newer" | "-type"
        )
    });
    if by_metadata {
        return block(
            "Use `sak fs find <path>` instead of `find` for metadata searches \
             (--size +1M, --mtime -7d, --type f|d|l, --name <glob>).",
        );
    }
    block(
        "Use `sak fs glob '<pattern>'` instead of `find` for name searches \
         (or `sak fs find <path>` to filter by --size/--mtime/--type).",
    )
}

fn check_jq(pos: &[&str]) -> Option<String> {
    // jq FILTER (stdin) is 1 positional; jq FILTER FILE is 2+.
    if pos.len() >= 2 {
        return block(
            "Use `sak json query <path> <file>` instead of `jq` for files. \
             Other ops: keys, flatten, grep, length, paths, schema, type, validate, diff.",
        );
    }
    None
}

fn check_yq(base: &str, pos: &[&str]) -> Option<String> {
    if pos.len() >= 2 {
        return block(&format!(
            "Use `sak config query <path> <file>` instead of `{base}` for files. \
             Handles TOML/YAML/JSON/plist."
        ));
    }
    None
}

fn check_openssl(args: &[String]) -> Option<String> {
    match args.first().map(|s| s.as_str()) {
        Some("x509") => block(
            "Use `sak cert inspect <cert>` instead of `openssl x509`. \
             Also: `sak cert expiring --days 30`, `sak cert from-kubeconfig`.",
        ),
        Some("dgst") => {
            block("Use `sak hash sha256|sha1|md5|blake3 <file>` instead of `openssl dgst`.")
        }
        _ => None,
    }
}

/// `*sum` / `b3sum` are read-only digest tools (their `-c`/`--check` mode maps
/// to `sak hash --verify`), so every invocation redirects to `sak hash`.
fn check_sum(base: &str) -> Option<String> {
    let algo = match base {
        "sha1sum" => "sha1",
        "md5sum" => "md5",
        "b3sum" => "blake3",
        // sha256sum and shasum (shasum defaults to SHA-1 but takes `-a 256`):
        // suggest sha256 and let the algo list cover the rest.
        _ => "sha256",
    };
    block(&format!(
        "Use `sak hash {algo} <file>` instead of `{base}` \
         (add `--verify <sumfile>` to check; other algos: sha256, sha1, md5, blake3)."
    ))
}

fn check_git(args: &[String]) -> Option<String> {
    // Strip git global flags to find the actual subcommand.
    let mut i = 0;
    while i < args.len() && args[i].starts_with('-') {
        let a = &args[i];
        if matches!(
            a.as_str(),
            "-C" | "-c" | "--git-dir" | "--work-tree" | "--namespace"
        ) && i + 1 < args.len()
        {
            i += 2;
        } else {
            i += 1;
        }
    }
    if i >= args.len() {
        return None;
    }
    let sub = args[i].as_str();
    let rest: Vec<&str> = args[i + 1..].iter().map(String::as_str).collect();

    match sub {
        "status" => block("Use `sak git status` instead of `git status`."),
        "diff" => block(
            "Use `sak git diff` (--staged, --name-only, --stat, --commit supported) \
             instead of `git diff`.",
        ),
        "log" => block(
            "Use `sak git log` (--oneline, -n, --author, --grep, --since, -- <path> supported) \
             instead of `git log`.",
        ),
        "show" => block(
            "Use `sak git show` (--stat, --name-only, --format supported) \
             instead of `git show`.",
        ),
        "blame" => block("Use `sak git blame` (-L 10,20 supported) instead of `git blame`."),
        "shortlog" => block("Use `sak git contributors` instead of `git shortlog`."),
        "branch" => {
            // Listing-only forms: no args, or only list-like flags.
            let list_flags = [
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
            if rest.is_empty() || rest.iter().all(|a| list_flags.contains(a)) {
                return block(
                    "Use `sak git branch` to list branches. \
                     (`git branch -d/-D/-m/-c/<name>` is allowed.)",
                );
            }
            None
        }
        "tag" => {
            // Listing-only forms.
            let list_ok = |a: &&str| {
                matches!(*a, "-l" | "--list" | "-n" | "--column" | "--no-column")
                    || a.starts_with("-n")
                    || a.starts_with("--sort")
            };
            if rest.is_empty() || rest.iter().all(list_ok) {
                return block(
                    "Use `sak git tags` to list tags. \
                     (`git tag -a/-d <name>` is allowed.)",
                );
            }
            None
        }
        "remote" => {
            if rest.is_empty()
                || matches!(
                    rest.first().copied(),
                    Some("-v" | "--verbose" | "show" | "get-url")
                )
            {
                return block(
                    "Use `sak git remote` to list remotes. \
                     (`git remote add/remove/set-url` is allowed.)",
                );
            }
            None
        }
        "stash" => {
            if rest.first().copied() == Some("list") {
                return block("Use `sak git stash-list` instead of `git stash list`.");
            }
            None
        }
        _ => None,
    }
}

fn check_kubectl(pos: &[&str]) -> Option<String> {
    match pos.first().copied() {
        Some("get") | Some("describe") | Some("logs") | Some("events") => {
            let sub = pos[0];
            block(&format!(
                "Use `sak k8s {sub}` instead of `kubectl {sub}`. \
                 Also: sak k8s failing/pending/restarts/images/contexts/kinds/schema."
            ))
        }
        // `kubectl api-resources` → `sak k8s kinds`
        Some("api-resources") => block("Use `sak k8s kinds` instead of `kubectl api-resources`."),
        // `kubectl explain` → `sak k8s schema`
        Some("explain") => {
            block("Use `sak k8s schema <group/version/Kind>` instead of `kubectl explain`.")
        }
        // `kubectl config get-contexts` → `sak k8s contexts`
        Some("config") if pos.get(1).copied() == Some("get-contexts") => {
            block("Use `sak k8s contexts` instead of `kubectl config get-contexts`.")
        }
        _ => None,
    }
}

fn check_talosctl(pos: &[&str]) -> Option<String> {
    match pos.first().copied() {
        Some("get") | Some("read") => {
            let sub = pos[0];
            block(&format!(
                "Use `sak talos {sub}` instead of `talosctl {sub}` \
                 (fans out across nodes; also `sak talos certs` for fleet cert inventory)."
            ))
        }
        _ => None,
    }
}

fn check_docker(pos: &[&str]) -> Option<String> {
    match pos.first().copied() {
        Some("ps") => block("Use `sak docker list` instead of `docker ps`."),
        Some("images") => block("Use `sak docker images` instead of `docker images`."),
        Some("inspect") => block(
            "Use `sak docker info <container>` or `sak docker config <container>` \
             instead of `docker inspect`.",
        ),
        _ => None,
    }
}

fn check_lxc(base: &str, pos: &[&str]) -> Option<String> {
    match pos.first().copied() {
        Some("list") => block(&format!("Use `sak lxc list` instead of `{base} list`.")),
        Some("info") => block(&format!(
            "Use `sak lxc info <instance>` instead of `{base} info`."
        )),
        Some("config") if pos.get(1).copied() == Some("show") => block(&format!(
            "Use `sak lxc config <instance>` instead of `{base} config show`."
        )),
        Some("image") if matches!(pos.get(1).copied(), Some("list") | Some("ls")) => block(
            &format!("Use `sak lxc images` instead of `{base} image list`."),
        ),
        _ => None,
    }
}

fn check_gh(args: &[String], pos: &[&str]) -> Option<String> {
    match (pos.first().copied(), pos.get(1).copied()) {
        // Only redirect GET reads. A non-GET `gh api` means the caller wants a
        // write that `sak gh` deliberately can't perform, so it passes through
        // to real `gh`. (Other read verbs get their own redirects as those
        // commands land.)
        (Some("api"), _) if gh_api_method_is_get(args) => {
            block("Use `sak gh api <endpoint>` instead of `gh api` for GET requests.")
        }
        (Some("pr"), Some("list")) => {
            block("Use `sak gh pr-list` instead of `gh pr list` (TSV/JSON, --fields forwarded).")
        }
        (Some("pr"), Some("view")) => {
            block("Use `sak gh pr-view <pr>` instead of `gh pr view` (JSON/TSV).")
        }
        (Some("issue"), Some("list")) => block(
            "Use `sak gh issue-list` instead of `gh issue list` (TSV/JSON, --fields forwarded).",
        ),
        (Some("issue"), Some("view")) => {
            block("Use `sak gh issue-view <issue>` instead of `gh issue view` (JSON/TSV).")
        }
        (Some("run"), Some("list")) => {
            block("Use `sak gh run-list` instead of `gh run list` (TSV/JSON, --fields forwarded).")
        }
        (Some("run"), Some("view")) => block(
            "Use `sak gh run-view <run-id>` instead of `gh run view` (JSON/TSV, or --log/--log-failed).",
        ),
        (Some("release"), Some("list")) => block(
            "Use `sak gh release-list` instead of `gh release list` (TSV/JSON, --fields forwarded).",
        ),
        (Some("release"), Some("view")) => {
            block("Use `sak gh release-view [<tag>]` instead of `gh release view` (JSON/TSV).")
        }
        (Some("workflow"), Some("list")) => block(
            "Use `sak gh workflow-list` instead of `gh workflow list` (TSV/JSON, --fields forwarded).",
        ),
        (Some("repo"), Some("view")) => {
            block("Use `sak gh repo-view [<owner/name>]` instead of `gh repo view` (JSON/TSV).")
        }
        _ => None,
    }
}

fn check_helm(pos: &[&str]) -> Option<String> {
    // `helm ls` is an alias for `helm list`. Reads gain redirects as their sak
    // commands land; the rest still pass through. Mutating verbs (`install`,
    // `upgrade`, `uninstall`, `repo add`, ...) are never redirected — `sak
    // helm` can't perform them.
    match pos.first().copied() {
        Some("list") | Some("ls") => block(
            "Use `sak helm list` instead of `helm list`/`helm ls` (TSV/JSON, --status/--filter/-A).",
        ),
        Some("status") => block(
            "Use `sak helm status <release>` instead of `helm status` (TSV/JSON, --revision).",
        ),
        Some("get") => block(
            "Use `sak helm get <release> --what all|manifest|values|notes|hooks` instead of `helm get`.",
        ),
        // Every `helm show`/`helm inspect` (deprecated alias) subcommand is a
        // read — chart/values/readme/crds/all.
        Some("show") | Some("inspect") => block(
            "Use `sak helm show <chart> --what all|chart|values|readme|crds` instead of `helm show`.",
        ),
        // `helm template` renders locally and never contacts the cluster.
        Some("template") => block(
            "Use `sak helm template <chart>` instead of `helm template` (offline render to YAML).",
        ),
        Some("lint") => {
            block("Use `sak helm lint <chart>` instead of `helm lint` (TSV findings + pass/fail).")
        }
        // Both `helm search repo` and `helm search hub` are reads.
        Some("search") => {
            block("Use `sak helm search <term> --source repo|hub` instead of `helm search`.")
        }
        Some("history") | Some("hist") => {
            block("Use `sak helm history <release>` instead of `helm history` (TSV/JSON, --max).")
        }
        // Only `repo list` is a read; `repo add`/`update`/`remove` are writes
        // sak can't perform, so they pass through.
        Some("repo") if pos.get(1).copied() == Some("list") => {
            block("Use `sak helm repo-list` instead of `helm repo list` (TSV/JSON).")
        }
        // `dependency` aliases: `dep`, `dependencies`. Only `list` is a read;
        // `dependency update`/`build` are writes (they fetch + write Chart.lock).
        Some("dependency") | Some("dependencies") | Some("dep")
            if pos.get(1).copied() == Some("list") =>
        {
            block(
                "Use `sak helm dependency-list <chart>` instead of `helm dependency list` (TSV/JSON).",
            )
        }
        _ => None,
    }
}

/// `nix` is hierarchical (`nix flake show`, `nix store info`, ...). Reads gain
/// redirects as their `sak nix` commands land; everything else — and every
/// mutating verb (`build`, `copy`, `store delete`, `profile install`, `flake
/// update`, ...) that `sak nix` can't perform — passes through. The first arg
/// is the verb, the second (when present) the subverb.
fn check_nix(args: &[String], pos: &[&str]) -> Option<String> {
    // `nix eval` is read-only-ish, and `sak nix eval` injects `--read-only`.
    // Redirect only the pure case: leave `--impure` / `--no-pure-eval` evals
    // alone, since the injected `--read-only` would change their semantics.
    if pos.first().copied() == Some("eval") {
        if args
            .iter()
            .any(|a| a == "--impure" || a == "--no-pure-eval")
        {
            return None;
        }
        return block(
            "Use `sak nix eval [installable] [--expr <e>] [-f <file>]` instead of `nix eval` \
             (read-only, --json/--raw, --apply).",
        );
    }
    check_nix_subverbs(pos)
}

/// `nix-store` is the second nix binary; only `--query` reads, and sak covers
/// the three reference queries. Redirect those; leave every other `--query`
/// sub-flag and all mutating operations (`--delete`, `--gc`, `--realise`, ...)
/// alone — sak can't perform them and doesn't shadow those reads yet.
fn check_nix_store(args: &[String]) -> Option<String> {
    let has = |f: &str| args.iter().any(|a| a == f);
    if has("--query") && (has("--references") || has("--referrers") || has("--requisites")) {
        return block(
            "Use `sak nix references <path>` (--referrers / --closure) instead of \
             `nix-store --query --references/--referrers/--requisites`.",
        );
    }
    None
}

fn check_nix_subverbs(pos: &[&str]) -> Option<String> {
    match (pos.first().copied(), pos.get(1).copied()) {
        (Some("flake"), Some("show")) => block(
            "Use `sak nix flake-show [flake-ref]` instead of `nix flake show` \
             (TSV output-path/type/description, --all-systems, --format json).",
        ),
        // `nix store ping` is the (deprecated) alias for `nix store info`.
        (Some("store"), Some("info")) | (Some("store"), Some("ping")) => block(
            "Use `sak nix store-info` instead of `nix store info`/`nix store ping` \
             (TSV url/version/trusted/..., --field, --store, --format json).",
        ),
        // Only `registry list` is a read; `registry add`/`remove`/`pin` mutate
        // the registry and pass through (sak nix can't perform them).
        (Some("registry"), Some("list")) => block(
            "Use `sak nix registry-list` instead of `nix registry list` \
             (TSV scope/from/to, --scope, --format json).",
        ),
        // Only `profile list` is a read; `profile install`/`remove`/`upgrade`/
        // `rollback`/`wipe-history` mutate the profile and pass through.
        (Some("profile"), Some("list")) => block(
            "Use `sak nix profile-list` instead of `nix profile list` \
             (TSV index/name/store-path/flake-attr, --profile, --format json).",
        ),
        // `nix derivation show` and the deprecated top-level `nix show-derivation`
        // alias both read; `nix derivation add` mutates and passes through.
        (Some("derivation"), Some("show")) | (Some("show-derivation"), _) => block(
            "Use `sak nix derivation-show [installable]` instead of `nix derivation show` \
             (JSON passthrough, --recursive).",
        ),
        _ => None,
    }
}

/// Whether a `gh api` invocation is an HTTP GET — true when no `-X` /
/// `--method` flag is present (gh's default), or its value is `GET`
/// (case-insensitive). Mirrors the chokepoint's method detection in
/// `src/gh/client.rs::check_api_method`.
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

fn check_sqlite(args: &[String]) -> Option<String> {
    let joined = args.join(" ").to_lowercase();
    let markers = [
        ".tables",
        ".schema",
        ".dump",
        ".indexes",
        ".databases",
        "select ",
    ];
    if markers.iter().any(|m| joined.contains(m)) {
        return block(
            "Use `sak sqlite tables/schema/count/query/dump/info <db>` \
             instead of `sqlite3` for reads.",
        );
    }
    None
}

fn check_sysctl(args: &[String]) -> Option<String> {
    // Mutations are out of scope for sak — allow them through. Setting a knob is
    // `key=value` (with or without `-w`); loading config files is `-p`/`--load`
    // or `--system`. Everything else (`sysctl`, `-a`, `<key>`) is a read.
    let is_write = args.iter().any(|a| {
        a.contains('=')
            || a == "-w"
            || a == "--write"
            || a == "-p"
            || a == "--load"
            || a == "--system"
    });
    if is_write {
        return None;
    }
    block("Use `sak linux sysctl [pattern]` instead of `sysctl` for reads.")
}
