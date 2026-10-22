//! Muse Code pre-tool-use hook.
//!
//! Reads the Muse Code `PreToolUse` JSON payload from stdin and decides
//! whether the about-to-run shell command should be redirected to a `sak`
//! equivalent. Exits 0 to allow, exits 2 with a stderr message to block.
//! Prefix the shell command with `SAK_HOOK_BYPASS=1` (or export it in the hook
//! process's own environment) to disable the hook for one call.
//!
//! The decision protocol is the same shape as Claude Code's: exit 0 allows,
//! exit 2 plus a stderr message blocks and the message is fed back to the
//! model. Both facts were verified against live `muse exec` probe runs, not
//! assumed from the Claude parity (see the capture issue's evidence comment).
//!
//! The classifier is shared, not forked: command strings go through
//! [`super::claude_code::classify`], so every domain's `HOOK_RULES` table
//! applies identically under both harnesses. The run driver is shared too
//! ([`super::run_hook`]) — only the payload envelope is Muse-specific:
//! `tool_name` arrives lowercase (`"bash"`, matched case-insensitively) and
//! the command lives at `tool_input.command`.
//!
//! Configure the project tier in `<project-root>/.muse/hooks.json`:
//!
//! ```json
//! {
//!   "hooks": {
//!     "PreToolUse": [
//!       {
//!         "matcher": "Bash",
//!         "hooks": [{ "type": "command", "command": "sak hook muse-code" }]
//!       }
//!     ]
//!   }
//! }
//! ```

use anyhow::Result;
use clap::Args;
use serde::Deserialize;

use crate::output::Outcome;

#[derive(Args)]
#[command(
    about = "Pre-tool-use hook for Muse Code",
    long_about = "Pre-tool-use hook for Muse Code.\n\n\
        Reads the harness's PreToolUse JSON payload from stdin. When the \
        about-to-run shell command has a read-only `sak` equivalent, exit 2 \
        with a stderr message naming the replacement (Muse Code surfaces \
        that message back to the model). All other commands pass through with \
        exit 0.\n\n\
        Classification reuses the shared shell-command engine, so a redirectable \
        read can't hide behind a pipeline, a substitution (`$(…)` / backticks), \
        a subshell or brace group, a `sh -c '…'` script, a wrapper prefix \
        (sudo/env/xargs/timeout/nice/…), or a `find … -exec …` clause.\n\n\
        Prefix the shell command with SAK_HOOK_BYPASS=1 to disable the hook for \
        one call. Exporting SAK_HOOK_BYPASS=1 into the hook process's own \
        environment works too.",
    after_help = "\
Configure the project tier in <project-root>/.muse/hooks.json:

  {
    \"hooks\": {
      \"PreToolUse\": [
        {
          \"matcher\": \"Bash\",
          \"hooks\": [{ \"type\": \"command\", \"command\": \"sak hook muse-code\" }]
        }
      ]
    }
  }

The user tier is the same `hooks` block in ~/.config/muse/settings.json
(schema_version 1); the managed tier is a file named by `managed_hooks_path`.
Start a new session to reload hook configuration.

Examples:
  sak hook muse-code                   Read JSON payload from stdin (normal use)
  sak hook muse-code --check 'git log' Test a command directly (no stdin)
  SAK_HOOK_BYPASS=1 git status         One-shot escape hatch"
)]
pub struct MuseCodeArgs {
    /// Classify this command string directly instead of reading stdin.
    /// Intended for debugging the rule set from the shell.
    #[arg(long, value_name = "COMMAND", conflicts_with = "emit_config")]
    pub check: Option<String>,
    /// Print a paste-ready hook configuration for one tier and exit:
    /// `project` (<project-root>/.muse/hooks.json), `user`
    /// (~/.config/muse/settings.json `hooks` block), or `managed` (a file
    /// named by `managed_hooks_path`). All three tiers share the same JSON
    /// shape; only the destination differs.
    #[arg(long, value_name = "TIER")]
    pub emit_config: Option<ConfigTier>,
}

/// One hook-configuration tier. The JSON shape is identical for all three —
/// verified tier by tier against `muse exec` startup validation (a malformed
/// file warns as `MalformedConfig` naming its source; a good one loads
/// silent) — so the tier only selects the destination hint on stderr.
#[derive(Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum ConfigTier {
    Project,
    User,
    Managed,
}

impl ConfigTier {
    /// Where the printed JSON goes. Shown on stderr so stdout stays pure
    /// JSON, safe to redirect straight into the destination file.
    fn hint(self) -> &'static str {
        match self {
            ConfigTier::Project => {
                "Write to <project-root>/.muse/hooks.json (project hooks run after you trust the folder), then start a new session."
            }
            ConfigTier::User => {
                "Merge the \"hooks\" block into ~/.config/muse/settings.json (keep \"schema_version\": 1), then start a new session."
            }
            ConfigTier::Managed => {
                "Write to the file named by the `managed_hooks_path` setting (whoever controls that file controls what executes), then start a new session."
            }
        }
    }
}

/// The canonical hook registration, shared byte-for-byte by every tier.
fn hook_config_json() -> serde_json::Value {
    serde_json::json!({
        "hooks": {
            "PreToolUse": [
                {
                    "matcher": "Bash",
                    "hooks": [{ "type": "command", "command": "sak hook muse-code" }]
                }
            ]
        }
    })
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

// Muse Code's pre-tool-use hook protocol matches Claude Code's shape:
//   - exit 0 → allow the tool call (we re-use Outcome::Found for its `0`)
//   - exit 2 → block + the stderr message is fed back to the model
//     (we re-use Outcome::Partial for its `2`)
// Both were verified against live `muse exec` probe runs: an exit-0 shim let
// `echo probe-hello` run, and an exit-2 shim blocked it pre-execution with its
// stderr message quoted back by the model.
pub fn run(args: &MuseCodeArgs) -> Result<Outcome> {
    if let Some(tier) = args.emit_config {
        println!("{}", serde_json::to_string_pretty(&hook_config_json())?);
        eprintln!("{}", tier.hint());
        return Ok(Outcome::Found);
    }

    super::run_hook(args.check.as_deref(), "muse-code", command_from_payload)
}

/// Extract the shell command from a raw Muse `PreToolUse` stdin payload.
/// Returns `None` when there is nothing to classify: malformed JSON, a
/// non-shell `tool_name`, or an empty command. All three are fail-open —
/// the caller maps `None` to allow.
fn command_from_payload(buf: &str) -> Option<String> {
    let payload: HookPayload = serde_json::from_str(buf).ok()?;
    // Muse sends `tool_name` lowercase (`"bash"`); the hooks.json `matcher`
    // that selects it is case-insensitive server-side, so match the tool
    // case-insensitively here too and never depend on one spelling.
    if !payload.tool_name.eq_ignore_ascii_case("bash") {
        return None;
    }
    if payload.tool_input.command.trim().is_empty() {
        return None;
    }
    Some(payload.tool_input.command)
}

#[cfg(test)]
mod tests {
    use super::super::claude_code::classify;
    use super::*;

    /// Minimal live-shaped payload; only the fields the adapter reads matter.
    fn payload(tool_name: &str, command: &str) -> String {
        serde_json::json!({
            "hook_event_name": "PreToolUse",
            "tool_name": tool_name,
            "tool_input": {"command": command},
            "tool_use_id": "call_probe",
        })
        .to_string()
    }

    #[test]
    fn lowercase_bash_tool_is_intercepted() {
        assert_eq!(
            command_from_payload(&payload("bash", "git status")),
            Some("git status".to_string())
        );
    }

    #[test]
    fn tool_name_match_is_case_insensitive() {
        // The live payload says "bash" but hooks.json matchers say "Bash";
        // the adapter must not depend on one spelling.
        assert_eq!(
            command_from_payload(&payload("Bash", "git status")),
            Some("git status".to_string())
        );
        assert_eq!(
            command_from_payload(&payload("BASH", "git status")),
            Some("git status".to_string())
        );
    }

    #[test]
    fn non_shell_tool_is_ignored() {
        assert_eq!(command_from_payload(&payload("read", "src/main.rs")), None);
        assert_eq!(command_from_payload(&payload("edit", "src/main.rs")), None);
    }

    #[test]
    fn malformed_json_fails_open() {
        assert_eq!(command_from_payload("not json at all"), None);
        assert_eq!(command_from_payload(""), None);
    }

    #[test]
    fn empty_command_fails_open() {
        assert_eq!(command_from_payload(&payload("bash", "")), None);
        assert_eq!(command_from_payload(&payload("bash", "   ")), None);
    }

    #[test]
    fn live_capture_shape_parses() {
        // The exact non-command fields from the first live probe run must not
        // break parsing if Muse adds more envelope around them.
        let buf = r#"{"hook_event_name":"PreToolUse","tool_name":"bash","tool_input":{"command":"echo probe-hello","description":"Echo probe-hello","workdir":"/private/tmp/muse-hook-probe"},"tool_use_id":"call_01a15c4af6f9745bb5e5c26a676996d7","session_id":"01a15c4a-e654-79c1-bd8a-8fb2d31a569b","turn_id":"01a15c4a-ec49-7391-a1b4-0955928ebd1b","cwd":"/private/tmp/muse-hook-probe","transcript_path":null,"model":"muse-wonder-1.0","permission_mode":"bypassPermissions"}"#;
        assert_eq!(
            command_from_payload(buf),
            Some("echo probe-hello".to_string())
        );
    }

    #[test]
    fn check_read_exits_block_and_write_exits_allow() {
        // Pins the exit-code mapping: swapping Found/Partial would silently
        // invert the hook. `run` prints the redirect to stderr on block.
        let block = MuseCodeArgs {
            check: Some("git status".to_string()),
            emit_config: None,
        };
        assert!(matches!(run(&block).unwrap(), Outcome::Partial));
        let allow = MuseCodeArgs {
            check: Some("git commit -m x".to_string()),
            emit_config: None,
        };
        assert!(matches!(run(&allow).unwrap(), Outcome::Found));
    }

    #[test]
    fn emitted_config_is_valid_hook_registration() {
        let v = hook_config_json();
        assert_eq!(v["hooks"]["PreToolUse"][0]["matcher"], "Bash");
        assert_eq!(
            v["hooks"]["PreToolUse"][0]["hooks"][0]["command"],
            "sak hook muse-code"
        );
        assert_eq!(v["hooks"]["PreToolUse"][0]["hooks"][0]["type"], "command");
    }

    #[test]
    fn every_tier_points_somewhere_distinct() {
        // The JSON is shared; the tier selects the destination hint.
        assert_ne!(ConfigTier::Project.hint(), ConfigTier::User.hint());
        assert_ne!(ConfigTier::Project.hint(), ConfigTier::Managed.hint());
        assert_ne!(ConfigTier::User.hint(), ConfigTier::Managed.hint());
    }

    #[test]
    fn shared_classifier_blocks_reads_through_muse_payload() {
        // Parity with the claude-code path: the same engine decides.
        let cmd = command_from_payload(&payload("bash", "git status")).unwrap();
        assert!(classify(&cmd).is_some());
        let cmd = command_from_payload(&payload("bash", "git commit -m x")).unwrap();
        assert!(classify(&cmd).is_none());
    }
}
