//! Sole chokepoint for invoking the `helm` binary.
//!
//! Every other module under `src/helm/` must call into the helpers exposed
//! here. Constructing a `std::process::Command` for `helm` from anywhere else
//! in the domain is forbidden, and the
//! [`tests::no_helm_invocations_outside_client_module`] grep test enforces it
//! on every `cargo test` run.
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

use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result, bail};

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
    ("verify", None),   // chart signature verification
    ("version", None),
    ("env", None),
    ("dependency", Some("list")),
    ("plugin", Some("list")),
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

/// Output of one `helm` invocation. Stdout is bytes (so binary-ish payloads
/// round-trip cleanly) and stderr is text (so error reporting is readable).
#[derive(Debug)]
pub struct Output {
    pub stdout: Vec<u8>,
    pub stderr: String,
    pub status: std::process::ExitStatus,
}

/// Invoke `helm [conn flags] <verb> [subverb] <args...>`.
///
/// The `(verb, subverb)` pair must be covered by [`READ_ONLY_VERBS`];
/// otherwise this returns an error without spawning a subprocess. Connection
/// flags are applied before the verb so they don't tangle with verb-specific
/// flags in `args`.
pub fn invoke(verb: &str, subverb: Option<&str>, args: &[&str], conn: Conn) -> Result<Output> {
    if !verb_allowed(verb, subverb) {
        bail!(
            "helm verb `{}` is not in the read-only allowlist ({})",
            display_verb(verb, subverb),
            allowlist_summary()
        );
    }

    let mut cmd = Command::new("helm");
    if let Some(kc) = conn.kubeconfig {
        cmd.arg("--kubeconfig").arg(kc);
    }
    if let Some(ns) = conn.namespace {
        cmd.arg("--namespace").arg(ns);
    }
    if let Some(ctx) = conn.context {
        cmd.arg("--kube-context").arg(ctx);
    }
    cmd.arg(verb);
    if let Some(sv) = subverb {
        cmd.arg(sv);
    }
    for a in args {
        cmd.arg(a);
    }

    let output = cmd
        .output()
        .with_context(|| "spawning `helm` (is it installed and on PATH?)".to_string())?;

    Ok(Output {
        stdout: output.stdout,
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        status: output.status,
    })
}

/// Convenience: run [`invoke`] and return stdout if the process exited 0. On
/// non-zero exit, surface stderr (trimmed) as an `anyhow::Error`.
pub fn invoke_ok(verb: &str, subverb: Option<&str>, args: &[&str], conn: Conn) -> Result<Vec<u8>> {
    let out = invoke(verb, subverb, args, conn)?;
    if !out.status.success() {
        let trimmed = out.stderr.trim();
        let suffix = if trimmed.is_empty() {
            String::new()
        } else {
            format!(": {}", trimmed)
        };
        bail!("helm {} failed{}", display_verb(verb, subverb), suffix);
    }
    Ok(out.stdout)
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
    let out = invoke(verb, subverb, args, conn)?;
    if out.status.success() {
        return Ok(Some(out.stdout));
    }
    let trimmed = out.stderr.trim();
    if trimmed.contains("not found") {
        return Ok(None);
    }
    let suffix = if trimmed.is_empty() {
        String::new()
    } else {
        format!(": {}", trimmed)
    };
    bail!("helm {} failed{}", display_verb(verb, subverb), suffix);
}

/// Whether `(verb, subverb)` is covered by the allowlist. A `None` entry for
/// `verb` admits any subverb (the subverb is treated as a read-only argument);
/// otherwise the provided subverb must equal a `Some(_)` entry exactly.
fn verb_allowed(verb: &str, subverb: Option<&str>) -> bool {
    READ_ONLY_VERBS.iter().any(|(v, sv)| {
        *v == verb
            && match (sv, subverb) {
                (None, _) => true,
                (Some(want), Some(got)) => *want == got,
                (Some(_), None) => false,
            }
    })
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

    /// The `Command::new(` constructor — banned strictly (no `hook.rs`
    /// exemption) because hook rules are pure data and never spawn
    /// subprocesses. Banning the name alone would leave the variable-built
    /// `Command` loophole; banning the constructor alone would let a
    /// non-helm `Command` be re-targeted. The two assertions together close
    /// every realistic shell-out path.
    const COMMAND_NEW_TOKEN: &[&str] = &["Command::new("];

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
    fn no_command_new_outside_client_module() {
        crate::test_support::assert_no_forbidden_tokens(
            "helm",
            COMMAND_NEW_TOKEN,
            "Command::new(...) must be confined to src/helm/client.rs",
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
    fn invoke_found_enforces_allowlist_before_spawn() {
        // invoke_found routes through invoke, so a mutating verb is rejected
        // by the allowlist without spawning helm (the not-found -> Ok(None)
        // mapping needs a live helm and is covered by command-level tests).
        let err = invoke_found("uninstall", None, &[], Conn::default()).unwrap_err();
        assert!(err.to_string().contains("not in the read-only allowlist"));
    }
}
