//! Chokepoint for invoking the `helm` binary.
//!
//! Every other module under `src/helm/` must call into the helpers exposed
//! here. Spawning itself is the shared [`crate::local_bin::LocalBinClient`]
//! (the tree-wide grep test confines all `Command::new(` to it); naming the
//! `helm` binary directly from anywhere else in the domain is forbidden, and
//! the [`tests::no_helm_name_token_outside_client_or_hook`] grep test
//! enforces that on every `cargo test` run.
//!
//! Read-only enforcement is convention plus a verb allowlist plus the grep
//! test. `helm` has plenty of mutating subcommands (`install`, `upgrade`,
//! `rollback`, `uninstall`, `repo add`, ...) so the chokepoint refuses to
//! invoke any (verb, subverb) pair not covered by [`READ_ONLY_VERBS`]. There
//! is no read-only flavor of `helm` itself, so the allowlist is the cheapest
//! credible defense — mirroring the talos and nix domains.
//!
//! The chokepoint does not interpret per-verb flags; commands assemble their
//! own arg vectors and pass them in. That keeps the trust boundary at the
//! verb level and avoids the chokepoint growing knowledge of every `helm`
//! subcommand's surface.

use std::ffi::OsString;
use std::path::Path;

use anyhow::{Result, bail};

use crate::local_bin::{LocalBinClient, pair_allowed, stderr_suffix};

pub use crate::local_bin::Output;

/// (verb, subverb) pairs of `helm` that this domain is allowed to invoke.
/// Anything not covered here is rejected at the chokepoint with a hard error —
/// no fallthrough, no env-var override.
///
/// A `None` subverb means the *whole verb family* is read-only — e.g. every
/// `helm get …` form (`all`, `manifest`, `values`, `notes`, `hooks`) reads,
/// so the subcommand word rides in `args`. A `Some(sv)` entry locks the verb
/// to that one read-only subverb: `helm repo list` is allowed but `helm repo
/// add` / `repo update` are not, because `repo` has no `None` entry.
///
/// Adding a new entry is a deliberate change: every verb here must be strictly
/// read-only (no cluster mutations, no writes to `~/.cache/helm` /
/// `repositories.yaml`, no on-disk artifacts). Re-check the verb's `helm
/// <verb> --help` output before extending the list.
pub const READ_ONLY_VERBS: &[(&str, Option<&str>)] = &[
    ("list", None),
    ("get", None), // helm get all/manifest/values/notes/hooks — all read
    ("status", None),
    ("history", None),
    ("show", None),     // show all/chart/values/readme/crds
    ("template", None), // pure local render, no cluster writes
    ("search", None),   // search repo / search hub
    ("lint", None),     // chart linting, no mutations
    ("dependency", Some("list")),
    ("repo", Some("list")),
];

/// Optional connection flags forwarded to `helm`. `helm` reads `KUBECONFIG` /
/// `~/.kube/config` directly (same as kubectl), so an all-`None` `Conn`
/// inherits the ambient environment with no flags added.
#[derive(Debug, Default, Clone, Copy)]
pub struct Conn<'a> {
    pub kubeconfig: Option<&'a Path>,
    pub namespace: Option<&'a str>,
    pub context: Option<&'a str>,
}

/// The single `helm` spawner. All subprocesses go through the shared
/// [`LocalBinClient`]; the `"helm"` literal lives here (and in `hook.rs`
/// tool names) by chokepoint-test exemption.
static HELM: LocalBinClient = LocalBinClient::new("helm");

/// Invoke `helm [conn flags] <verb> [subverb] <args...>`.
///
/// The `(verb, subverb)` pair must be covered by [`READ_ONLY_VERBS`];
/// otherwise this returns an error without spawning a subprocess. Connection
/// flags are applied before the verb so they don't tangle with verb-specific
/// flags in `args`.
pub fn invoke(verb: &str, subverb: Option<&str>, args: &[&str], conn: Conn) -> Result<Output> {
    ensure_allowed(verb, subverb)?;
    HELM.run(&build_argv(verb, subverb, args, conn))
}

/// Convenience: run [`invoke`] and return stdout if the process exited 0. On
/// non-zero exit, surface stderr (trimmed) as an `anyhow::Error`.
pub fn invoke_ok(verb: &str, subverb: Option<&str>, args: &[&str], conn: Conn) -> Result<Vec<u8>> {
    ensure_allowed(verb, subverb)?;
    HELM.run_ok(
        &format!("helm {}", display_verb(verb, subverb)),
        &build_argv(verb, subverb, args, conn),
    )
}

/// Like [`invoke_ok`] but maps helm's "not found" failure (a named release /
/// revision that doesn't exist) to `Ok(None)`, so single-release commands can
/// produce sak's exit code 1 for "no such release" without losing the ability
/// to surface other failures as exit code 2. Mirrors `k8s::client::get_dyn`.
///
/// helm reports a missing release on stderr as `Error: release: not found`
/// (and a missing revision similarly), so the discriminator is the substring
/// `not found`. Any other non-zero exit is surfaced as an error.
pub fn invoke_found(
    verb: &str,
    subverb: Option<&str>,
    args: &[&str],
    conn: Conn,
) -> Result<Option<Vec<u8>>> {
    ensure_allowed(verb, subverb)?;
    let out = HELM.run(&build_argv(verb, subverb, args, conn))?;
    if out.status.success() {
        return Ok(Some(out.stdout));
    }
    if out.stderr.trim().contains("not found") {
        return Ok(None);
    }
    bail!(
        "helm {} failed{}",
        display_verb(verb, subverb),
        stderr_suffix(&out.stderr)
    );
}

/// Reject a `(verb, subverb)` pair not covered by [`READ_ONLY_VERBS`] before
/// any subprocess spawns. Shared by [`invoke`], [`invoke_ok`], and
/// [`invoke_found`] so the allowlist can't be bypassed by picking the wrong
/// entry point.
fn ensure_allowed(verb: &str, subverb: Option<&str>) -> Result<()> {
    if !verb_allowed(verb, subverb) {
        bail!(
            "helm verb `{}` is not in the read-only allowlist ({})",
            display_verb(verb, subverb),
            allowlist_summary()
        );
    }
    Ok(())
}

/// Assemble the `helm [conn flags] <verb> [subverb] <args...>` argv.
/// Connection flags lead so they don't tangle with verb-specific flags.
fn build_argv(verb: &str, subverb: Option<&str>, args: &[&str], conn: Conn) -> Vec<OsString> {
    let mut argv: Vec<OsString> = Vec::new();
    if let Some(kc) = conn.kubeconfig {
        argv.push("--kubeconfig".into());
        argv.push(kc.into());
    }
    if let Some(ns) = conn.namespace {
        argv.push("--namespace".into());
        argv.push(ns.into());
    }
    if let Some(ctx) = conn.context {
        argv.push("--kube-context".into());
        argv.push(ctx.into());
    }
    argv.push(verb.into());
    if let Some(sv) = subverb {
        argv.push(sv.into());
    }
    argv.extend(args.iter().map(OsString::from));
    argv
}

/// Whether `(verb, subverb)` is covered by the allowlist. Matching semantics
/// are the shared [`pair_allowed`](crate::local_bin::pair_allowed): a `None`
/// entry for `verb` admits any subverb (the subverb is treated as a read-only
/// argument); otherwise the provided subverb must equal a `Some(_)` entry
/// exactly.
fn verb_allowed(verb: &str, subverb: Option<&str>) -> bool {
    pair_allowed(READ_ONLY_VERBS, verb, subverb)
}

fn display_verb(verb: &str, subverb: Option<&str>) -> String {
    match subverb {
        Some(sv) => format!("{} {}", verb, sv),
        None => verb.to_string(),
    }
}

fn allowlist_summary() -> String {
    READ_ONLY_VERBS
        .iter()
        .map(|(v, sv)| display_verb(v, *sv))
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Binary-name string: its only legitimate appearance outside
    /// `client.rs` is as the `tool:` field of a [`crate::hook::rule::HookRule`]
    /// in `src/helm/hook.rs` and inside that file's static redirect messages
    /// (which mention "helm" by design), so that file is exempt.
    const HELM_NAME_TOKEN: &[&str] = &["\"helm\""];

    /// `Command::new(` must appear nowhere in this domain — not even in
    /// `client.rs`: all spawning goes through the shared `LocalBinClient`
    /// (see the tree-wide `no_shell_spawns_outside_shared_chokepoint`
    /// test). Banning the constructor alongside the binary name closes the
    /// variable-built `Command` loophole as well as direct `helm` use.
    const COMMAND_NEW_TOKEN: &[&str] = &[crate::test_support::COMMAND_NEW];

    #[test]
    fn no_helm_name_token_outside_client_or_hook() {
        crate::test_support::assert_no_forbidden_tokens_except(
            "helm",
            HELM_NAME_TOKEN,
            &["client.rs", "hook.rs"],
            "the \"helm\" name literal must be confined to client.rs (chokepoint) \
             or hook.rs (HookRule.tool fields + redirect messages)",
        );
    }

    #[test]
    fn no_command_new_in_domain() {
        // No exemption, client.rs included: the token is built with
        // `concat!`, so this file doesn't match its own test.
        crate::test_support::assert_no_forbidden_tokens_except(
            "helm",
            COMMAND_NEW_TOKEN,
            &[],
            "no src/helm/ file may reference the Command constructor; \
             all spawning goes through the shared LocalBinClient",
        );
    }

    #[test]
    fn rejects_mutating_verb() {
        for (verb, subverb) in [
            ("install", None),
            ("upgrade", None),
            ("uninstall", None),
            ("rollback", None),
            ("repo", Some("add")),
            ("repo", Some("update")),
            ("dependency", Some("update")),
        ] {
            let err = invoke(verb, subverb, &[], Conn::default()).unwrap_err();
            assert!(
                err.to_string().contains("not in the read-only allowlist"),
                "{} should be rejected, got: {err}",
                display_verb(verb, subverb)
            );
        }
    }

    #[test]
    fn admits_read_only_verbs() {
        // Whole-family verbs admit any subverb (rides in args).
        assert!(verb_allowed("get", Some("manifest")));
        assert!(verb_allowed("get", None));
        assert!(verb_allowed("list", None));
        assert!(verb_allowed("show", Some("values")));
        // Pinned-subverb verbs admit only their one read subverb.
        assert!(verb_allowed("repo", Some("list")));
        assert!(!verb_allowed("repo", Some("add")));
        assert!(!verb_allowed("repo", None));
        assert!(verb_allowed("dependency", Some("list")));
        assert!(!verb_allowed("dependency", Some("build")));
        // Unknown verbs are rejected outright.
        assert!(!verb_allowed("install", None));
    }

    #[test]
    fn rejects_allowlist_entries_without_commands() {
        // `verify`, `version`, `env`, and `plugin list` were dropped from the
        // allowlist: no sak command stands behind them (sak-llm-5svf).
        // Re-adding any of them must come with the command, not silently.
        for (verb, subverb) in [
            ("verify", None),
            ("version", None),
            ("env", None),
            ("plugin", Some("list")),
        ] {
            let err = invoke(verb, subverb, &[], Conn::default()).unwrap_err();
            assert!(
                err.to_string().contains("not in the read-only allowlist"),
                "{} should be rejected, got: {err}",
                display_verb(verb, subverb)
            );
        }
    }

    #[test]
    fn invoke_found_enforces_allowlist_before_spawn() {
        // invoke_found shares the ensure_allowed gate, so a mutating verb is
        // rejected by the allowlist without spawning helm (the not-found ->
        // Ok(None) mapping needs a live helm and is covered by
        // command-level tests).
        let err = invoke_found("uninstall", None, &[], Conn::default()).unwrap_err();
        assert!(err.to_string().contains("not in the read-only allowlist"));
    }
}
