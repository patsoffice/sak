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
pub mod rule;

#[cfg(test)]
mod tests;

use std::process::ExitCode;

use anyhow::Result;
use clap::Subcommand;

#[derive(Subcommand)]
pub enum HookCommand {
    /// Pre-tool-use hook for Claude Code (claude.com/claude-code).
    #[command(name = "claude-code")]
    ClaudeCode(claude_code::ClaudeCodeArgs),
}

pub fn run(cmd: &HookCommand) -> Result<ExitCode> {
    match cmd {
        HookCommand::ClaudeCode(args) => claude_code::run(args),
    }
}
