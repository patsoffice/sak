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

    /// Tokens that must not appear in any `src/helm/*.rs` file other than
    /// `client.rs`. The directory walk and comment-skip mechanics live in
    /// [`crate::test_support::assert_no_forbidden_tokens`].
    ///
    /// Two strings cover the surface: the literal binary name as a quoted
    /// string ("helm") and the `Command::new(` constructor. Either alone would
    /// leave loopholes (spawn helm via a `Command` built from a variable, or
    /// build a non-helm `Command::new` then re-target it). Together, every
    /// realistic shell-out path trips the test.
    const FORBIDDEN_TOKENS: &[&str] = &["\"helm\"", "Command::new("];

    #[test]
    fn no_helm_invocations_outside_client_module() {
        crate::test_support::assert_no_forbidden_tokens(
            "helm",
            FORBIDDEN_TOKENS,
            "helm invocations / Command::new must be confined to src/helm/client.rs",
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
}
