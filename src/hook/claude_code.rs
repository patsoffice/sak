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

use anyhow::Result;
use clap::Args;
use serde::Deserialize;

use crate::output::Outcome;

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

// Claude Code's pre-tool-use hook protocol uses exit codes as decision
// signals, not as result-found indicators:
//   - exit 0 → allow the tool call (we re-use Outcome::Found for its `0`)
//   - exit 2 → block + the stderr message is fed back to the model
//     (we re-use Outcome::Partial for its `2`)
// The variant names are the wrong shape for hook semantics — nothing was
// "found" or "partial". They're picked here purely for their exit_code()
// mapping. The alternative would have been a fourth Outcome variant just
// for hook, which we explicitly didn't add.
pub fn run(args: &ClaudeCodeArgs) -> Result<Outcome> {
    if std::env::var("SAK_HOOK_BYPASS").as_deref() == Ok("1") {
        return Ok(Outcome::Found);
    }

    let command = match &args.check {
        Some(cmd) => cmd.clone(),
        None => {
            let mut buf = String::new();
            io::stdin().read_to_string(&mut buf)?;
            // Empty stdin → nothing to check.
            if buf.trim().is_empty() {
                return Ok(Outcome::Found);
            }
            let payload: HookPayload = match serde_json::from_str(&buf) {
                Ok(p) => p,
                // Malformed JSON shouldn't block real work — fail open.
                Err(_) => return Ok(Outcome::Found),
            };
            // Only intercept Bash tool calls.
            if payload.tool_name != "Bash" {
                return Ok(Outcome::Found);
            }
            payload.tool_input.command
        }
    };

    if command.trim().is_empty() {
        return Ok(Outcome::Found);
    }

    if let Some(msg) = classify(&command) {
        eprintln!("{}", msg);
        return Ok(Outcome::Partial);
    }

    Ok(Outcome::Found)
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

/// Aggregate every domain's `HOOK_RULES` table. The always-on domains
/// (`fs`, `git`, `json`, `config`, `cert`, `hash`, `nix`, `gh`, `helm`,
/// `talos`, `linux`) are listed unconditionally; the cargo-feature-gated
/// domains (`k8s`, `docker`, `lxc`, `sqlite`) are `#[cfg]` per-element so a
/// `--no-default-features` binary drops their rules entirely — it never
/// suggests a command it doesn't ship.
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
        #[cfg(feature = "k8s")]
        crate::k8s::hook::HOOK_RULES,
        #[cfg(feature = "docker")]
        crate::docker::hook::HOOK_RULES,
        #[cfg(feature = "lxc")]
        crate::lxc::hook::HOOK_RULES,
        #[cfg(feature = "sqlite")]
        crate::sqlite::hook::HOOK_RULES,
    ]
}

/// True when some registry owns `tool`. Test-only invariant helper now that
/// the legacy fallback is gone: the engine no longer branches on ownership
/// (every classify goes through [`check_registries`]), but tests still want
/// to assert which tools are wired in.
#[cfg(test)]
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

/// Classify a single command's tokens (post env-stripping). With every
/// domain migrated, this is now a thin wrapper over the registry engine —
/// the legacy per-tool `check_*` fallback that used to sit here is gone.
fn check(tokens: &[String]) -> Option<String> {
    let cmd_base = basename(&tokens[0]);
    check_registries(cmd_base, &tokens[1..])
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
    fn always_on_domain_tools_are_owned() {
        // Tools from always-on domains are in the registry regardless of
        // cargo features.
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
    }

    /// Feature-gated tools are owned only when their cargo feature is on.
    /// The matching pair below covers the lean-build path. This is the
    /// invariant the whole registry epic exists to enable: a
    /// `--no-default-features` binary doesn't suggest commands it doesn't
    /// ship.
    #[cfg(feature = "k8s")]
    #[test]
    fn k8s_tool_owned_when_feature_on() {
        assert!(tool_in_registries("kubectl"));
    }
    #[cfg(not(feature = "k8s"))]
    #[test]
    fn k8s_tool_not_owned_when_feature_off() {
        assert!(!tool_in_registries("kubectl"));
    }

    #[cfg(feature = "docker")]
    #[test]
    fn docker_tool_owned_when_feature_on() {
        assert!(tool_in_registries("docker"));
    }
    #[cfg(not(feature = "docker"))]
    #[test]
    fn docker_tool_not_owned_when_feature_off() {
        assert!(!tool_in_registries("docker"));
    }

    #[cfg(feature = "lxc")]
    #[test]
    fn lxc_tools_owned_when_feature_on() {
        assert!(tool_in_registries("lxc"));
        assert!(tool_in_registries("incus"));
    }
    #[cfg(not(feature = "lxc"))]
    #[test]
    fn lxc_tools_not_owned_when_feature_off() {
        assert!(!tool_in_registries("lxc"));
        assert!(!tool_in_registries("incus"));
    }

    #[cfg(feature = "sqlite")]
    #[test]
    fn sqlite_tool_owned_when_feature_on() {
        assert!(tool_in_registries("sqlite3"));
    }
    #[cfg(not(feature = "sqlite"))]
    #[test]
    fn sqlite_tool_not_owned_when_feature_off() {
        assert!(!tool_in_registries("sqlite3"));
    }
}
