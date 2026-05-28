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

use super::rule::{self, HookRule};

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

/// Aggregate every domain's `HOOK_RULES` table. Domains migrate into this list
/// one at a time; until a tool appears in some registry it falls back to the
/// legacy per-tool `check_*` below.
fn registries() -> &'static [&'static [HookRule]] {
    &[
        crate::fs::hook::HOOK_RULES,
        crate::git::hook::HOOK_RULES,
        crate::json::hook::HOOK_RULES,
        crate::config::hook::HOOK_RULES,
        crate::cert::hook::HOOK_RULES,
        crate::hash::hook::HOOK_RULES,
        crate::nix::hook::HOOK_RULES,
        crate::gh::hook::HOOK_RULES,
        crate::helm::hook::HOOK_RULES,
        crate::talos::hook::HOOK_RULES,
        crate::linux::hook::HOOK_RULES,
    ]
}

/// True when some registry owns `tool`. A registry-owned tool's registry result
/// is authoritative (block *or* allow) and never falls through to `check_*`, so
/// a domain migrates by moving its rows into a registry and deleting its
/// `check_*` arm in the same change.
fn tool_in_registries(tool: &str) -> bool {
    registries()
        .iter()
        .flat_map(|reg| reg.iter())
        .any(|r| r.tool == tool)
}

/// Apply the registries' rules for `tool` to `args`, returning the first
/// matching rule's message wrapped with the bypass hint. Split from
/// [`tool_in_registries`] so the engine can be unit-tested against a synthetic
/// registry without touching the global table.
fn check_registries(tool: &str, args: &[String]) -> Option<String> {
    apply_registries(registries(), tool, args)
}

fn apply_registries(regs: &[&[HookRule]], tool: &str, args: &[String]) -> Option<String> {
    let normalized = normalize_args(tool, args);
    let pos = positionals(&normalized);
    for reg in regs {
        for r in reg.iter() {
            if r.tool != tool {
                continue;
            }
            if !rule::subcommand_matches(r.subcommand, &pos) {
                continue;
            }
            if let Some(guard) = r.guard
                && !guard(&normalized)
            {
                continue;
            }
            return block(r.message);
        }
    }
    None
}

/// Tool-specific argument normalization applied before subcommand/guard
/// matching. Only `git` needs it today — its global flags precede the
/// subcommand — and every other tool is identity.
fn normalize_args(tool: &str, args: &[String]) -> Vec<String> {
    match tool {
        "git" => strip_git_global_flags(args),
        _ => args.to_vec(),
    }
}

/// Drop git's global flags (`-C <dir>`, `-c <k=v>`, `--git-dir <d>`,
/// `--work-tree <d>`, `--namespace <n>`) that precede the subcommand, returning
/// the args from the subcommand onward. Shared by the registry engine and the
/// legacy `check_git` so the two agree on where the subcommand starts.
fn strip_git_global_flags(args: &[String]) -> Vec<String> {
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
    args[i..].to_vec()
}

fn check(tokens: &[String]) -> Option<String> {
    let cmd_base = basename(&tokens[0]);
    let args: &[String] = &tokens[1..];

    // Per-domain registries take precedence. A registry-owned tool never falls
    // through to the legacy check_* below.
    if tool_in_registries(cmd_base) {
        return check_registries(cmd_base, args);
    }

    let pos = positionals(args);

    match cmd_base {
        "kubectl" => check_kubectl(&pos),
        "docker" => check_docker(&pos),
        "lxc" | "incus" => check_lxc(cmd_base, &pos),
        "sqlite3" => check_sqlite(args),
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

#[cfg(test)]
mod engine_tests {
    //! Engine-level tests for the declarative-registry path. The global
    //! `registries()` is empty in the foundation, so these drive
    //! `apply_registries` against a synthetic table to prove subcommand
    //! matching, guards, and per-tool normalization work before any domain
    //! migrates. End-to-end coverage of the real (still-legacy) rules lives in
    //! `super::super::tests`.
    use super::*;

    fn args(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|s| s.to_string()).collect()
    }

    fn has_no_dashes(args: &[String]) -> bool {
        args.iter().all(|a| !a.starts_with('-'))
    }

    const DEMO: &[HookRule] = &[
        HookRule {
            tool: "demo",
            subcommand: &[&["list"], &["ls"]],
            guard: None,
            message: "Use `sak demo list`.",
        },
        // Conditional rule: only fires when no flags are present.
        HookRule {
            tool: "demo",
            subcommand: &[&["plain"]],
            guard: Some(has_no_dashes),
            message: "Use `sak demo plain`.",
        },
    ];

    #[test]
    fn subcommand_alternatives_block_and_carry_message() {
        assert_eq!(
            apply_registries(&[DEMO], "demo", &args(&["list"])),
            Some(format!("Use `sak demo list`.{BYPASS_HINT}"))
        );
        assert!(apply_registries(&[DEMO], "demo", &args(&["ls", "-A"])).is_some());
    }

    #[test]
    fn unmatched_subcommand_returns_none() {
        assert!(apply_registries(&[DEMO], "demo", &args(&["status"])).is_none());
        // A different tool is never matched by this registry.
        assert!(apply_registries(&[DEMO], "other", &args(&["list"])).is_none());
    }

    #[test]
    fn guard_gates_the_match() {
        assert!(apply_registries(&[DEMO], "demo", &args(&["plain"])).is_some());
        // Same subcommand, but the guard rejects the flagged form.
        assert!(apply_registries(&[DEMO], "demo", &args(&["plain", "--force"])).is_none());
    }

    #[test]
    fn git_normalization_strips_global_flags_before_matching() {
        const G: &[HookRule] = &[HookRule {
            tool: "git",
            subcommand: &[&["status"]],
            guard: None,
            message: "Use `sak git status`.",
        }];
        // `-C /tmp` precedes the subcommand; normalize_args drops it so the
        // `status` prefix still matches.
        assert!(apply_registries(&[G], "git", &args(&["-C", "/tmp", "status"])).is_some());
        assert!(apply_registries(&[G], "git", &args(&["status"])).is_some());
    }

    #[test]
    fn migrated_tools_are_registry_owned_others_fall_back() {
        // Migrated domains' tools are registry-owned and skip the legacy
        // fallback; tools whose domains have not migrated still are not.
        assert!(tool_in_registries("cat"));
        assert!(tool_in_registries("tree"));
        assert!(tool_in_registries("git"));
        assert!(tool_in_registries("jq"));
        assert!(tool_in_registries("yq"));
        assert!(tool_in_registries("plistutil"));
        // openssl is split across cert (x509) and hash (dgst); either suffices.
        assert!(tool_in_registries("openssl"));
        assert!(tool_in_registries("sha256sum"));
        assert!(tool_in_registries("b3sum"));
        assert!(tool_in_registries("nix"));
        assert!(tool_in_registries("nix-store"));
        assert!(tool_in_registries("gh"));
        assert!(tool_in_registries("helm"));
        assert!(tool_in_registries("talosctl"));
        assert!(tool_in_registries("sysctl"));
        assert!(!tool_in_registries("kubectl"));
        assert!(!tool_in_registries("sqlite3"));
    }
}
