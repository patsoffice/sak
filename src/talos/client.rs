//! Chokepoint for invoking the `talosctl` binary.
//!
//! Every other module under `src/talos/` must call into the helpers exposed
//! here. Spawning itself is the shared [`crate::local_bin::LocalBinClient`]
//! (the tree-wide grep test confines all `Command::new(` to it); naming the
//! `talosctl` binary directly from anywhere else in the domain is forbidden,
//! and the
//! [`tests::no_talosctl_name_token_outside_client_or_hook`] grep test
//! enforces it on every `cargo test` run.
//!
//! Read-only enforcement is convention plus a verb allowlist plus the grep
//! test. `talosctl` has plenty of mutating subcommands (`reboot`, `reset`,
//! `apply-config`, `etcd snapshot restore`, ...) so the chokepoint refuses to
//! invoke any verb not on [`READ_ONLY_VERBS`]. There is no read-only flavor
//! of `talosctl` itself, so the allowlist is the cheapest credible defense.
//!
//! The chokepoint also does not interpret per-verb flags; commands assemble
//! their own arg vectors and pass them in. That keeps the trust boundary at
//! the verb level and avoids the chokepoint growing knowledge of every
//! `talosctl` subcommand's surface.

use std::ffi::OsString;
use std::path::Path;

use anyhow::{Result, bail};

use crate::local_bin::{LocalBinClient, stderr_suffix};

pub use crate::local_bin::Output;

/// Subcommands of `talosctl` that this domain is allowed to invoke. Anything
/// not on this list is rejected at the chokepoint with a hard error — no
/// fallthrough, no env-var override.
///
/// Adding a new entry is a deliberate change: every verb here must be
/// strictly read-only against the cluster (no mutations to node config, no
/// reboots, no restores, no service control). Re-check the verb's `talosctl
/// <verb> --help` output before extending the list.
pub const READ_ONLY_VERBS: &[&str] = &["get", "read", "version"];

/// The single `talosctl` spawner. All subprocesses go through the shared
/// [`LocalBinClient`]; the `"talosctl"` literal lives here (and in
/// `hook.rs` tool names) by chokepoint-test exemption.
static TALOSCTL: LocalBinClient = LocalBinClient::new("talosctl");

/// Invoke `talosctl <verb> <args...>` with optional connection flags.
///
/// `verb` must be a member of [`READ_ONLY_VERBS`]; otherwise this returns an
/// error without spawning a subprocess.
///
/// `node` adds `-n <ip>`. `talosconfig` adds `--talosconfig <path>`. Both are
/// applied before `verb` so they don't tangle with verb-specific flags in
/// `args`. Spawning itself is the shared [`LocalBinClient::run`].
pub fn invoke(
    verb: &str,
    args: &[&str],
    node: Option<&str>,
    talosconfig: Option<&Path>,
) -> Result<Output> {
    if !READ_ONLY_VERBS.contains(&verb) {
        bail!(
            "talosctl verb `{}` is not in the read-only allowlist ({})",
            verb,
            READ_ONLY_VERBS.join(", ")
        );
    }

    let mut argv: Vec<OsString> = Vec::new();
    if let Some(cfg) = talosconfig {
        argv.push("--talosconfig".into());
        argv.push(cfg.into());
    }
    if let Some(n) = node {
        argv.push("-n".into());
        argv.push(n.into());
    }
    argv.push(verb.into());
    argv.extend(args.iter().map(OsString::from));

    TALOSCTL.run(&argv)
}

/// Convenience: run `invoke` and return stdout if the process exited 0.
/// On non-zero exit, surface stderr (trimmed) as an `anyhow::Error` —
/// callers that need to distinguish "node missing file" from "node
/// unreachable" should inspect stderr themselves via [`invoke`].
pub fn invoke_ok(
    verb: &str,
    args: &[&str],
    node: Option<&str>,
    talosconfig: Option<&Path>,
) -> Result<Vec<u8>> {
    let out = invoke(verb, args, node, talosconfig)?;
    if !out.status.success() {
        let node_suffix = node.map(|n| format!(" on node {}", n)).unwrap_or_default();
        bail!(
            "talosctl {}{} failed{}",
            verb,
            node_suffix,
            stderr_suffix(&out.stderr)
        );
    }
    Ok(out.stdout)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Binary-name string: its only legitimate appearance outside
    /// `client.rs` is as the `tool:` field of a [`crate::hook::rule::HookRule`]
    /// in `src/talos/hook.rs` and inside that file's static redirect messages
    /// (which mention "talosctl" by design), so that file is exempt.
    const TALOSCTL_NAME_TOKEN: &[&str] = &["\"talosctl\""];

    /// The `Command::new(` constructor — banned strictly (no `hook.rs`
    /// exemption) because hook rules are pure data and never spawn
    /// subprocesses. Banning the name alone would leave the variable-built
    /// `Command` loophole; banning the constructor alone would let a
    /// non-talosctl `Command` be re-targeted. Spawning itself lives in
    /// the shared `LocalBinClient`, so the constructor is banned domain-wide
    /// here too — the tree-wide chokepoint test is the backstop.
    const COMMAND_NEW_TOKEN: &[&str] = &[crate::test_support::COMMAND_NEW];

    #[test]
    fn no_talosctl_name_token_outside_client_or_hook() {
        crate::test_support::assert_no_forbidden_tokens_except(
            "talos",
            TALOSCTL_NAME_TOKEN,
            &["client.rs", "hook.rs"],
            "the \"talosctl\" name literal must be confined to client.rs (chokepoint) \
             or hook.rs (HookRule.tool fields + redirect messages)",
        );
    }

    #[test]
    fn no_command_new_in_domain() {
        // No exemption, client.rs included: the token is built with
        // `concat!`, so this file doesn't match its own test.
        crate::test_support::assert_no_forbidden_tokens_except(
            "talos",
            COMMAND_NEW_TOKEN,
            &[],
            "no src/talos/ file may reference the Command constructor; \
             all spawning goes through the shared LocalBinClient",
        );
    }

    #[test]
    fn rejects_mutating_verb() {
        let err = invoke("reboot", &[], None, None).unwrap_err();
        assert!(err.to_string().contains("not in the read-only allowlist"));

        let err = invoke("apply-config", &[], None, None).unwrap_err();
        assert!(err.to_string().contains("not in the read-only allowlist"));

        let err = invoke("reset", &[], None, None).unwrap_err();
        assert!(err.to_string().contains("not in the read-only allowlist"));
    }
}
