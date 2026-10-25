//! Agent hook entrypoints.
//!
//! Each subcommand exposes a hook for a specific LLM agent harness. The hook
//! reads the harness's pre-tool-use payload from stdin (or an explicit
//! `--check` string), classifies the command, and exits with a decision the
//! harness understands:
//!
//! - exit 0 → allow the command to run
//! - exit 2 + stderr message → block; the message is fed back to the model
//! - exit 0 + "ask" JSON on stdout (Claude Code) → the user approves or
//!   denies a blocked command that carries the inline bypass marker
//!
//! Classification is pure command-string work with no network access. The
//! one disk write is [`bypass_log`]'s append to sak's own state file when a
//! model asks to bypass.

mod bypass_log;
pub mod claude_code;
pub mod muse_code;
pub mod rule;
mod shell_text;

#[cfg(test)]
mod tests;

use std::io::{self, IsTerminal, Read};

use anyhow::Result;
use clap::Subcommand;

use crate::output::Outcome;

#[derive(Subcommand)]
pub enum HookCommand {
    /// Pre-tool-use hook for Claude Code (claude.com/claude-code).
    #[command(name = "claude-code")]
    ClaudeCode(claude_code::ClaudeCodeArgs),
    /// Pre-tool-use hook for Muse Code.
    #[command(name = "muse-code")]
    MuseCode(muse_code::MuseCodeArgs),
}

pub fn run(cmd: &HookCommand) -> Result<Outcome> {
    match cmd {
        HookCommand::ClaudeCode(args) => claude_code::run(args),
        HookCommand::MuseCode(args) => muse_code::run(args),
    }
}

/// Shared pre-tool-use run driver for the hook adapters (`claude-code`,
/// `muse-code`).
///
/// Both harnesses share the decision skeleton: env-var bypass, `--check`
/// passthrough, TTY fail-open, stdin read, empty/malformed fail-open,
/// empty-command allow, [`claude_code::decide`] → allow / block / escalate.
/// Only the stdin payload envelope differs, so each adapter passes its own
/// `extract_command` (stdin text → shell command; `None` means fail open) and
/// its subcommand name for the TTY hint. A third harness adds an extractor,
/// not a third copy of this flow — the TTY fail-open once had to be fixed in
/// both files identically.
///
/// `ask` renders the harness's "ask the user" response for an escalated
/// command (printed to stdout with exit 0). A harness without one passes
/// `None`, and an escalation is refused like a block: the model wrote the
/// marker, so it never gets a silent pass. Escalations from a real payload
/// are logged; `--check` runs are debugging and aren't.
pub(crate) fn run_hook(
    check: Option<&str>,
    harness: &str,
    extract_command: impl FnOnce(&str) -> Option<String>,
    ask: Option<fn(&str) -> String>,
) -> Result<Outcome> {
    if std::env::var("SAK_HOOK_BYPASS").is_ok_and(|v| claude_code::is_bypass_value(&v)) {
        return Ok(Outcome::Found);
    }

    let command = match check {
        Some(cmd) => cmd.to_string(),
        None => {
            // A bare terminal invocation would block forever waiting for EOF
            // that never comes — and a harness always pipes, never attaches a
            // TTY — so fail open with a hint instead of hanging.
            if io::stdin().is_terminal() {
                eprintln!(
                    "sak hook {harness} reads its PreToolUse payload from stdin (or use --check <COMMAND>); nothing to check — allowing."
                );
                return Ok(Outcome::Found);
            }
            let mut buf = String::new();
            io::stdin().read_to_string(&mut buf)?;
            // Empty stdin → nothing to check.
            if buf.trim().is_empty() {
                return Ok(Outcome::Found);
            }
            match extract_command(&buf) {
                Some(cmd) => cmd,
                // Malformed JSON or a non-shell tool shouldn't block real
                // work — fail open.
                None => return Ok(Outcome::Found),
            }
        }
    };

    if command.trim().is_empty() {
        return Ok(Outcome::Found);
    }

    match claude_code::decide(&command) {
        claude_code::Decision::Allow => Ok(Outcome::Found),
        claude_code::Decision::Block(msg) => {
            eprintln!("{msg}");
            Ok(Outcome::Partial)
        }
        claude_code::Decision::Escalate(msg) => {
            if check.is_none() {
                bypass_log::record(harness, &command, &msg);
            }
            match ask {
                Some(render) => {
                    println!("{}", render(&msg));
                    Ok(Outcome::Found)
                }
                None => {
                    eprintln!("{}", refused_bypass_message(&msg));
                    Ok(Outcome::Partial)
                }
            }
        }
    }
}

/// What a model hears when it writes the bypass marker under a harness with
/// no way to ask the user: the redirect again, and that the call is theirs.
fn refused_bypass_message(rule_message: &str) -> String {
    format!(
        "{rule_message} SAK_HOOK_BYPASS is reserved for the user. If sak can't do \
         what you need, say what's missing and ask the user to run the command."
    )
}
