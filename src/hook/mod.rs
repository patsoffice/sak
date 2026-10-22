//! Agent hook entrypoints.
//!
//! Each subcommand exposes a hook for a specific LLM agent harness. The hook
//! reads the harness's pre-tool-use payload from stdin (or an explicit
//! `--check` string), classifies the command, and exits with a decision the
//! harness understands:
//!
//! - exit 0 → allow the command to run
//! - exit 2 + stderr message → block; the message is fed back to the model
//!
//! No subcommand here makes any change to disk or the network. This domain is
//! pure command-string classification.

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
/// empty-command allow, `classify` → block. Only the stdin payload envelope
/// differs, so each adapter passes its own `extract_command` (stdin text →
/// shell command; `None` means fail open) and its subcommand name for the
/// TTY hint. A third harness adds an extractor, not a third copy of this
/// flow — the TTY fail-open once had to be fixed in both files identically.
pub(crate) fn run_hook(
    check: Option<&str>,
    harness: &str,
    extract_command: impl FnOnce(&str) -> Option<String>,
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

    if let Some(msg) = claude_code::classify(&command) {
        eprintln!("{msg}");
        return Ok(Outcome::Partial);
    }

    Ok(Outcome::Found)
}
