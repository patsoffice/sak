//! Sole chokepoint for invoking the `nix` binary.
//!
//! Every other module under `src/nix/` must call into the helpers exposed
//! here. Constructing a `std::process::Command` for `nix` from anywhere else
//! in the domain is forbidden, and the
//! [`tests::no_nix_invocations_outside_client_module`] grep test enforces it
//! on every `cargo test` run.
//!
//! Read-only enforcement is convention plus a verb allowlist plus the grep
//! test. `nix` has plenty of mutating subcommands (`build`, `copy`, `store
//! delete`, `profile install`, `flake update`, ...) so the chokepoint refuses
//! to invoke any (verb, subverb) pair not covered by [`READ_ONLY_VERBS`].
//! There is no read-only flavor of `nix` itself, so the allowlist is the
//! cheapest credible defense — mirroring the talos and helm domains.
//!
//! Two extra hardening steps are unique to nix:
//!
//! 1. **`nix-command flakes` experimental features** are injected on every
//!    invocation via `--extra-experimental-features`, so the domain works on
//!    stock nix where these are still gated.
//! 2. **`eval`** can evaluate arbitrary expressions that perform import-from
//!    -derivation or otherwise touch the store; the chokepoint injects
//!    `--read-only` for it unconditionally so callers can't forget.
//!
//! The chokepoint does not interpret per-verb flags otherwise; commands
//! assemble their own arg vectors and pass them in. That keeps the trust
//! boundary at the verb level and avoids the chokepoint growing knowledge of
//! every `nix` subcommand's surface.

use std::process::Command;

use anyhow::{Context, Result, bail};

/// (verb, subverb) pairs of `nix` that this domain is allowed to invoke.
/// Anything not covered here is rejected at the chokepoint with a hard error —
/// no fallthrough, no env-var override.
///
/// A `None` subverb means the *whole verb family* is read-only — e.g.
/// `nix path-info`, `nix eval`, and `nix why-depends` take their arguments
/// directly with no read/write subverb split. A `Some(sv)` entry locks the
/// verb to that one read-only subverb: `nix flake show` is allowed but `nix
/// flake update` is not, because `flake` has no `None` entry.
///
/// Adding a new entry is a deliberate change: every verb here must be strictly
/// read-only (no builds, no store writes, no profile/registry mutations, no
/// flake.lock rewrites). Re-check the verb's `nix <verb> --help` output before
/// extending the list.
pub const READ_ONLY_VERBS: &[(&str, Option<&str>)] = &[
    ("flake", Some("show")),
    ("flake", Some("metadata")),
    ("flake", Some("info")),
    ("path-info", None),
    ("derivation", Some("show")),
    ("profile", Some("list")),
    ("registry", Some("list")),
    ("eval", None),
    ("store", Some("info")),
    ("store", Some("ls")),
    ("why-depends", None),
];

/// Output of one `nix` invocation. Stdout is bytes (so binary-ish payloads
/// round-trip cleanly) and stderr is text (so error reporting is readable).
#[derive(Debug)]
pub struct Output {
    pub stdout: Vec<u8>,
    pub stderr: String,
    pub status: std::process::ExitStatus,
}

/// Invoke `nix --extra-experimental-features '…' <verb> [subverb] <args...>`.
///
/// The `(verb, subverb)` pair must be covered by [`READ_ONLY_VERBS`];
/// otherwise this returns an error without spawning a subprocess.
///
/// `nix-command flakes` experimental features are injected unconditionally so
/// the domain works on stock nix. For `eval`, `--read-only` is injected too so
/// expression evaluation can't write to the store.
pub fn invoke(verb: &str, subverb: Option<&str>, args: &[&str]) -> Result<Output> {
    if !verb_allowed(verb, subverb) {
        bail!(
            "nix verb `{}` is not in the read-only allowlist ({})",
            display_verb(verb, subverb),
            allowlist_summary()
        );
    }

    let mut cmd = Command::new("nix");
    cmd.arg("--extra-experimental-features")
        .arg("nix-command flakes");
    cmd.arg(verb);
    if let Some(sv) = subverb {
        cmd.arg(sv);
    }
    // `nix eval` evaluates arbitrary expressions; force read-only so it can't
    // realise derivations or otherwise write to the store.
    if verb == "eval" {
        cmd.arg("--read-only");
    }
    for a in args {
        cmd.arg(a);
    }

    let output = cmd
        .output()
        .with_context(|| "spawning `nix` (is it installed and on PATH?)".to_string())?;

    Ok(Output {
        stdout: output.stdout,
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        status: output.status,
    })
}

/// Convenience: run [`invoke`] and return stdout if the process exited 0. On
/// non-zero exit, surface stderr (trimmed) as an `anyhow::Error`.
pub fn invoke_ok(verb: &str, subverb: Option<&str>, args: &[&str]) -> Result<Vec<u8>> {
    let out = invoke(verb, subverb, args)?;
    if !out.status.success() {
        let trimmed = out.stderr.trim();
        let suffix = if trimmed.is_empty() {
            String::new()
        } else {
            format!(": {}", trimmed)
        };
        bail!("nix {} failed{}", display_verb(verb, subverb), suffix);
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

    /// Tokens that must not appear in any `src/nix/*.rs` file other than
    /// `client.rs`. The directory walk and comment-skip mechanics live in
    /// [`crate::test_support::assert_no_forbidden_tokens`].
    ///
    /// Two strings cover the surface: the literal binary name as a quoted
    /// string ("nix") and the `Command::new(` constructor. Either alone would
    /// leave loopholes (spawn nix via a `Command` built from a variable, or
    /// build a non-nix `Command::new` then re-target it). Together, every
    /// realistic shell-out path trips the test.
    const FORBIDDEN_TOKENS: &[&str] = &["\"nix\"", "Command::new("];

    #[test]
    fn no_nix_invocations_outside_client_module() {
        crate::test_support::assert_no_forbidden_tokens(
            "nix",
            FORBIDDEN_TOKENS,
            "nix invocations / Command::new must be confined to src/nix/client.rs",
        );
    }

    #[test]
    fn rejects_mutating_verb() {
        for (verb, subverb) in [
            ("build", None),
            ("copy", None),
            ("store", Some("delete")),
            ("profile", Some("install")),
            ("flake", Some("update")),
        ] {
            let err = invoke(verb, subverb, &[]).unwrap_err();
            assert!(
                err.to_string().contains("not in the read-only allowlist"),
                "{} should be rejected, got: {err}",
                display_verb(verb, subverb)
            );
        }
    }

    #[test]
    fn admits_read_only_verbs() {
        // Whole-family verbs admit any subverb (rides in args) and the
        // no-subverb form.
        assert!(verb_allowed("path-info", None));
        assert!(verb_allowed("eval", None));
        assert!(verb_allowed("why-depends", None));
        // Pinned-subverb verbs admit only their listed read subverbs.
        assert!(verb_allowed("flake", Some("show")));
        assert!(verb_allowed("flake", Some("metadata")));
        assert!(!verb_allowed("flake", Some("update")));
        assert!(!verb_allowed("flake", None));
        assert!(verb_allowed("store", Some("info")));
        assert!(verb_allowed("store", Some("ls")));
        assert!(!verb_allowed("store", Some("delete")));
        assert!(verb_allowed("profile", Some("list")));
        assert!(!verb_allowed("profile", Some("install")));
        // Unknown verbs are rejected outright.
        assert!(!verb_allowed("build", None));
    }
}
