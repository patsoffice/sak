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
        The classifier recurses through shell composition, so a redirectable \
        read can't hide behind a pipeline, a substitution (`$(…)` / backticks), \
        a subshell or brace group, a `sh -c '…'` script, a wrapper prefix \
        (sudo/env/xargs/timeout/nice/…), or a `find … -exec …` clause.\n\n\
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

/// Cap on how deep the classifier recurses through nested composition
/// (substitutions, subshells, `sh -c` scripts, wrapper chains). A real command
/// nests a handful of levels at most; the bound only exists to make pathological
/// input (e.g. `sh -c 'sh -c "sh -c ..."'`) terminate.
const MAX_DEPTH: usize = 12;

/// Classify a full command string. Recurses through shell composition so a read
/// hidden inside a substitution (`$(…)`, backticks), a subshell / brace group
/// (`(…)`, `{ …; }`), a `sh -c '…'` script, a wrapper prefix (`sudo`, `env`,
/// `xargs`, `timeout`, …), or a `find … -exec …` clause is still caught. At each
/// level it splits on shell separators (`|`, `||`, `&&`, `;`, `&`) while
/// respecting quotes, then evaluates each piece. First block wins.
pub(crate) fn classify(command: &str) -> Option<String> {
    classify_depth(command, 0)
}

fn classify_depth(command: &str, depth: usize) -> Option<String> {
    if depth > MAX_DEPTH {
        return None;
    }
    // Reads hidden inside command substitutions / subshell / brace groups don't
    // appear as the leading token of any pipeline segment, so recurse into them
    // first.
    for inner in extract_nested(command) {
        if let Some(msg) = classify_depth(&inner, depth + 1) {
            return Some(msg);
        }
    }
    for part in split_pipeline(command) {
        let tokens = tokenize(&part);
        let tokens = strip_env_assignments(tokens);
        if tokens.is_empty() {
            continue;
        }
        if let Some(msg) = check(&tokens, depth) {
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

/// Classify a single command's tokens (post env-stripping). The common path is
/// a thin wrapper over the registry engine, but a leading shell (`sh -c …`),
/// command-prefix wrapper (`sudo`/`env`/`xargs`/`timeout`/…), or `find … -exec`
/// is peeled and its inner command re-classified so a redirectable read can't
/// hide behind a wrapper.
fn check(tokens: &[String], depth: usize) -> Option<String> {
    if tokens.is_empty() || depth > MAX_DEPTH {
        return None;
    }
    let cmd_base = basename(&tokens[0]);

    // `sh -c '<script>'`: the script operand is itself a command string.
    if is_shell_wrapper(cmd_base) {
        return shell_c_arg(&tokens[1..]).and_then(|script| classify_depth(script, depth + 1));
    }

    // `find … -exec <cmd> … {} ;|+`: the exec'd command lives mid-arglist. If it
    // names a redirectable read, block on that; otherwise fall through so find's
    // own search rules (`-name`/`-size`/…) still apply.
    if cmd_base == "find" {
        if let Some(cmd) = find_exec_command(&tokens[1..])
            && let Some(msg) = check(&cmd, depth + 1)
        {
            return Some(msg);
        }
        return check_registries(cmd_base, &tokens[1..]);
    }

    // `sudo grep …`, `xargs grep …`, `timeout 5 cat …`: peel the wrapper and
    // its own options, then re-classify what it runs.
    if let Some(mut rest) = strip_command_wrapper(cmd_base, &tokens[1..]) {
        // `xargs` appends file operands from stdin to the command it runs, so a
        // bare `xargs cat` does read files even though no path is written out.
        // Model that with a synthetic operand so file-requiring guards fire.
        if cmd_base == "xargs" {
            rest.push(XARGS_OPERAND.to_string());
        }
        return check(&rest, depth + 1);
    }

    check_registries(cmd_base, &tokens[1..])
}

/// Shells whose `-c` operand is a command string to recurse into. A bare
/// `bash script.sh` (no `-c`) runs a file, not an inline read, so it declines.
fn is_shell_wrapper(base: &str) -> bool {
    matches!(
        base,
        "sh" | "bash" | "zsh" | "dash" | "ash" | "ksh" | "mksh"
    )
}

/// The script operand of a shell `-c` invocation: the token right after a `-c`
/// flag (or a `-…c…` bundle like `-ec`/`-lc`). Returns `None` when there's no
/// `-c` (a script-file or interactive invocation — nothing inline to classify).
fn shell_c_arg(args: &[String]) -> Option<&str> {
    for (i, a) in args.iter().enumerate() {
        let is_c_flag = a == "-c"
            || (a.starts_with('-') && !a.starts_with("--") && a.contains('c') && a.len() > 1);
        if is_c_flag {
            return args.get(i + 1).map(String::as_str);
        }
    }
    None
}

/// Synthetic positional appended to an `xargs`-wrapped command so guards that
/// require a file operand (cat/grep/wc/…) recognize the stdin-fed input xargs
/// will append. Never matched as a real path — only counted as a positional.
const XARGS_OPERAND: &str = "__sak_xargs_operand__";

/// A command-prefix wrapper: a leading word whose later operands form the real
/// command. `arg_flags` are the wrapper's own options that consume the following
/// token as a value; `skip_positionals` is how many leading positional operands
/// precede the command (e.g. `timeout`'s DURATION).
struct Wrapper {
    name: &'static str,
    arg_flags: &'static [&'static str],
    skip_positionals: usize,
}

const WRAPPERS: &[Wrapper] = &[
    Wrapper {
        name: "sudo",
        arg_flags: &[
            "-u", "--user", "-g", "--group", "-C", "-p", "--prompt", "-T", "-R", "-D", "--chdir",
        ],
        skip_positionals: 0,
    },
    Wrapper {
        name: "env",
        arg_flags: &["-u", "--unset", "-C", "--chdir", "-S", "--split-string"],
        skip_positionals: 0,
    },
    Wrapper {
        name: "xargs",
        arg_flags: &[
            "-I",
            "-i",
            "--replace",
            "-n",
            "--max-args",
            "-L",
            "-l",
            "--max-lines",
            "-P",
            "--max-procs",
            "-d",
            "--delimiter",
            "-s",
            "--max-chars",
            "-E",
            "-e",
            "--eof",
            "-a",
            "--arg-file",
        ],
        skip_positionals: 0,
    },
    Wrapper {
        name: "timeout",
        arg_flags: &["-s", "--signal", "-k", "--kill-after"],
        skip_positionals: 1,
    },
    Wrapper {
        name: "nice",
        arg_flags: &["-n", "--adjustment"],
        skip_positionals: 0,
    },
    Wrapper {
        name: "time",
        arg_flags: &["-o", "--output", "-f", "--format"],
        skip_positionals: 0,
    },
    Wrapper {
        name: "ionice",
        arg_flags: &["-c", "--class", "-n", "--classdata"],
        skip_positionals: 0,
    },
    Wrapper {
        name: "stdbuf",
        arg_flags: &["-i", "--input", "-o", "--output", "-e", "--error"],
        skip_positionals: 0,
    },
    Wrapper {
        name: "watch",
        arg_flags: &["-n", "--interval"],
        skip_positionals: 0,
    },
    Wrapper {
        name: "nohup",
        arg_flags: &[],
        skip_positionals: 0,
    },
    Wrapper {
        name: "setsid",
        arg_flags: &[],
        skip_positionals: 0,
    },
    Wrapper {
        name: "command",
        arg_flags: &[],
        skip_positionals: 0,
    },
];

/// Peel a known command-prefix wrapper, returning the inner command's tokens
/// (its own options and `skip_positionals` leading operands removed, plus any
/// `VAR=val` assignments the wrapper passes through). `None` when `base` isn't a
/// wrapper or nothing follows it.
fn strip_command_wrapper(base: &str, args: &[String]) -> Option<Vec<String>> {
    let w = WRAPPERS.iter().find(|w| w.name == base)?;
    let mut i = 0;
    let mut skipped = 0;
    while i < args.len() {
        let a = &args[i];
        if a == "--" {
            i += 1;
            break;
        }
        if a.starts_with('-') && a.len() > 1 {
            if w.arg_flags.contains(&a.as_str()) && i + 1 < args.len() {
                i += 2;
            } else {
                i += 1;
            }
            continue;
        }
        if skipped < w.skip_positionals {
            skipped += 1;
            i += 1;
            continue;
        }
        break;
    }
    let rest = strip_env_assignments(args[i..].to_vec());
    if rest.is_empty() { None } else { Some(rest) }
}

/// Extract the command run by `find … -exec <cmd> … ;|+` (or `-execdir`/`-ok`/
/// `-okdir`): the tokens after the action flag up to the `;`/`+` terminator. The
/// `{}` placeholder is kept — it stands in for the matched path, so leaving it as
/// a positional lets file-requiring guards (cat/grep/…) recognize the read.
/// `None` when there's no exec clause.
fn find_exec_command(args: &[String]) -> Option<Vec<String>> {
    const EXEC_FLAGS: &[&str] = &["-exec", "-execdir", "-ok", "-okdir"];
    let start = args.iter().position(|a| EXEC_FLAGS.contains(&a.as_str()))?;
    let mut cmd = Vec::new();
    for a in &args[start + 1..] {
        if a == ";" || a == "+" {
            break;
        }
        cmd.push(a.clone());
    }
    if cmd.is_empty() { None } else { Some(cmd) }
}

/// Extract the inner command strings of command substitutions (`$(…)`,
/// backticks) and subshell / brace-group blocks (`(…)`, `{ …; }`) at the top
/// level of `cmd`, quote-aware. Nested blocks surface when the returned strings
/// are themselves re-scanned on recursion, so only the outermost layer is
/// collected here. Unbalanced delimiters are left alone (fail open).
fn extract_nested(cmd: &str) -> Vec<String> {
    let bytes = cmd.as_bytes();
    let n = bytes.len();
    let mut out = Vec::new();
    let mut i = 0;
    let mut in_single = false;
    let mut in_double = false;

    while i < n {
        let b = bytes[i];
        if in_single {
            if b == b'\'' {
                in_single = false;
            }
            i += 1;
            continue;
        }
        if in_double {
            // Single quotes are inert inside double quotes, but `$(…)` and
            // backticks still expand, so keep scanning for them.
            if b == b'\\' && i + 1 < n {
                i += 2;
                continue;
            }
            if b == b'"' {
                in_double = false;
                i += 1;
                continue;
            }
        } else {
            match b {
                b'\'' => {
                    in_single = true;
                    i += 1;
                    continue;
                }
                b'"' => {
                    in_double = true;
                    i += 1;
                    continue;
                }
                b'\\' if i + 1 < n => {
                    i += 2;
                    continue;
                }
                _ => {}
            }
        }
        // `$(…)` command substitution (also reachable inside double quotes).
        if b == b'$'
            && i + 1 < n
            && bytes[i + 1] == b'('
            && let Some((inner, end)) = balanced(bytes, i + 1, b'(', b')')
        {
            out.push(inner);
            i = end;
            continue;
        }
        // Backtick substitution (also reachable inside double quotes).
        if b == b'`'
            && let Some((inner, end)) = backtick_span(bytes, i)
        {
            out.push(inner);
            i = end;
            continue;
        }
        // Subshell, only at the unquoted top level.
        if !in_double
            && b == b'('
            && let Some((inner, end)) = balanced(bytes, i, b'(', b')')
        {
            out.push(inner);
            i = end;
            continue;
        }
        // A brace group opens with `{` followed by whitespace; `{a,b}` brace
        // expansion and `${VAR}` don't, so they're left alone.
        if !in_double
            && b == b'{'
            && i + 1 < n
            && (bytes[i + 1] as char).is_whitespace()
            && let Some((inner, end)) = balanced(bytes, i, b'{', b'}')
        {
            out.push(inner);
            i = end;
            continue;
        }
        i += 1;
    }
    out
}

/// Return the contents between a balanced `open`/`close` pair starting at
/// `bytes[start] == open`, plus the index just past the closing delimiter.
/// Quote- and escape-aware; honors nesting. `None` if never balanced.
fn balanced(bytes: &[u8], start: usize, open: u8, close: u8) -> Option<(String, usize)> {
    let n = bytes.len();
    let mut depth = 0i32;
    let mut i = start;
    let mut in_single = false;
    let mut in_double = false;
    let inner_start = start + 1;

    while i < n {
        let b = bytes[i];
        if in_single {
            if b == b'\'' {
                in_single = false;
            }
            i += 1;
            continue;
        }
        if in_double {
            if b == b'\\' && i + 1 < n {
                i += 2;
                continue;
            }
            if b == b'"' {
                in_double = false;
            }
            i += 1;
            continue;
        }
        if b == b'\\' && i + 1 < n {
            i += 2;
            continue;
        }
        if b == b'\'' {
            in_single = true;
        } else if b == b'"' {
            in_double = true;
        } else if b == open {
            depth += 1;
        } else if b == close {
            depth -= 1;
            if depth == 0 {
                let inner = String::from_utf8_lossy(&bytes[inner_start..i]).into_owned();
                return Some((inner, i + 1));
            }
        }
        i += 1;
    }
    None
}

/// Return a backtick span's contents (starting at the opening backtick) plus the
/// index past the closing backtick. Backslash-aware; `None` if unterminated.
fn backtick_span(bytes: &[u8], start: usize) -> Option<(String, usize)> {
    let n = bytes.len();
    let inner_start = start + 1;
    let mut i = inner_start;
    while i < n {
        let b = bytes[i];
        if b == b'\\' && i + 1 < n {
            i += 2;
            continue;
        }
        if b == b'`' {
            let inner = String::from_utf8_lossy(&bytes[inner_start..i]).into_owned();
            return Some((inner, i + 1));
        }
        i += 1;
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

    // ── composition helpers ───────────────────────────────────────

    #[test]
    fn shell_c_arg_finds_script_after_c_flag() {
        assert_eq!(shell_c_arg(&args(&["-c", "cat x"])), Some("cat x"));
        // Flag bundles that include `c` still point at the next operand.
        assert_eq!(shell_c_arg(&args(&["-lc", "grep y"])), Some("grep y"));
        assert_eq!(shell_c_arg(&args(&["-ec", "grep y"])), Some("grep y"));
        // No `-c`: a script-file / interactive shell has nothing inline.
        assert!(shell_c_arg(&args(&["script.sh"])).is_none());
        assert!(shell_c_arg(&args(&[])).is_none());
        // `--` long flags must not be mistaken for a `-c` bundle.
        assert!(shell_c_arg(&args(&["--rcfile", "f"])).is_none());
    }

    #[test]
    fn strip_command_wrapper_peels_options_and_positionals() {
        // Plain wrapper: inner command is everything after the name.
        assert_eq!(
            strip_command_wrapper("xargs", &args(&["grep", "foo"])),
            Some(args(&["grep", "foo"]))
        );
        // xargs option that consumes its value (`-n 1`) is skipped.
        assert_eq!(
            strip_command_wrapper("xargs", &args(&["-n", "1", "grep", "foo"])),
            Some(args(&["grep", "foo"]))
        );
        // timeout skips one leading positional (the DURATION).
        assert_eq!(
            strip_command_wrapper("timeout", &args(&["5", "cat", "x"])),
            Some(args(&["cat", "x"]))
        );
        assert_eq!(
            strip_command_wrapper("timeout", &args(&["-s", "KILL", "5", "cat", "x"])),
            Some(args(&["cat", "x"]))
        );
        // env passes through VAR=val before the command.
        assert_eq!(
            strip_command_wrapper("env", &args(&["FOO=bar", "grep", "x"])),
            Some(args(&["grep", "x"]))
        );
        // Not a wrapper / nothing to run.
        assert!(strip_command_wrapper("kubectl", &args(&["get", "pods"])).is_none());
        assert!(strip_command_wrapper("nohup", &args(&[])).is_none());
    }

    #[test]
    fn find_exec_command_extracts_inner_command() {
        assert_eq!(
            find_exec_command(&args(&[
                ".", "-type", "f", "-exec", "grep", "foo", "{}", "+"
            ])),
            Some(args(&["grep", "foo", "{}"]))
        );
        assert_eq!(
            find_exec_command(&args(&[".", "-execdir", "cat", "{}", ";"])),
            Some(args(&["cat", "{}"]))
        );
        // No exec clause → nothing to recurse into.
        assert!(find_exec_command(&args(&[".", "-name", "*.rs"])).is_none());
    }

    #[test]
    fn extract_nested_pulls_substitutions_and_groups() {
        assert_eq!(extract_nested("echo $(cat x)"), vec!["cat x".to_string()]);
        assert_eq!(extract_nested("echo `cat x`"), vec!["cat x".to_string()]);
        assert_eq!(
            extract_nested("( grep foo bar )"),
            vec![" grep foo bar ".to_string()]
        );
        assert_eq!(
            extract_nested("{ grep foo; }"),
            vec![" grep foo; ".to_string()]
        );
        // `$(…)` expands even inside double quotes.
        assert_eq!(
            extract_nested("echo \"x: $(cat y)\""),
            vec!["cat y".to_string()]
        );
        // …but not inside single quotes.
        assert!(extract_nested("echo '$(cat y)'").is_empty());
        // Brace expansion / `${VAR}` are not command groups.
        assert!(extract_nested("echo {a,b}").is_empty());
        assert!(extract_nested("echo ${HOME}").is_empty());
        // Unbalanced delimiters fail open (no panic, no extraction).
        assert!(extract_nested("echo $(cat x").is_empty());
    }
}
