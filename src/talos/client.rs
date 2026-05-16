//! Sole chokepoint for invoking the `talosctl` binary.
//!
//! Every other module under `src/talos/` must call into the helpers exposed
//! here. Constructing a `std::process::Command` for `talosctl` from anywhere
//! else in the domain is forbidden, and the
//! [`tests::no_talosctl_invocations_outside_client_module`] grep test
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

use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result, bail};

/// Subcommands of `talosctl` that this domain is allowed to invoke. Anything
/// not on this list is rejected at the chokepoint with a hard error — no
/// fallthrough, no env-var override.
///
/// Adding a new entry is a deliberate change: every verb here must be
/// strictly read-only against the cluster (no mutations to node config, no
/// reboots, no restores, no service control). Re-check the verb's `talosctl
/// <verb> --help` output before extending the list.
pub const READ_ONLY_VERBS: &[&str] = &["get", "read", "version"];

/// Output of one `talosctl` invocation. Stdout is bytes (so binary file
/// reads via `talosctl read` round-trip cleanly) and stderr is text (so
/// error reporting is readable).
#[derive(Debug)]
pub struct Output {
    pub stdout: Vec<u8>,
    pub stderr: String,
    pub status: std::process::ExitStatus,
}

/// Invoke `talosctl <verb> <args...>` with optional connection flags.
///
/// `verb` must be a member of [`READ_ONLY_VERBS`]; otherwise this returns an
/// error without spawning a subprocess.
///
/// `node` adds `-n <ip>`. `talosconfig` adds `--talosconfig <path>`. Both are
/// applied before `verb` so they don't tangle with verb-specific flags in
/// `args`.
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

    let mut cmd = Command::new("talosctl");
    if let Some(cfg) = talosconfig {
        cmd.arg("--talosconfig").arg(cfg);
    }
    if let Some(n) = node {
        cmd.arg("-n").arg(n);
    }
    cmd.arg(verb);
    for a in args {
        cmd.arg(a);
    }

    let output = cmd
        .output()
        .with_context(|| "spawning `talosctl` (is it installed and on PATH?)".to_string())?;

    Ok(Output {
        stdout: output.stdout,
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        status: output.status,
    })
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
        let trimmed = out.stderr.trim();
        let suffix = if trimmed.is_empty() {
            String::new()
        } else {
            format!(": {}", trimmed)
        };
        let node_suffix = node.map(|n| format!(" on node {}", n)).unwrap_or_default();
        bail!("talosctl {}{} failed{}", verb, node_suffix, suffix);
    }
    Ok(out.stdout)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsStr;
    use std::fs;
    use std::path::PathBuf;

    /// Tokens that must not appear in any `src/talos/*.rs` file other than
    /// `client.rs`. Comments are exempt — the skip logic below ignores any
    /// line whose first non-whitespace characters are `//`.
    ///
    /// Two strings cover the surface: the literal binary name as a quoted
    /// string ("talosctl") and the `Command::new(` constructor. Either alone
    /// would leave loopholes (you could spawn talosctl by building the
    /// `Command` from a variable, or build a non-talosctl `Command::new` and
    /// then later re-target it). Together, every realistic shell-out path
    /// trips the test.
    const FORBIDDEN_TOKENS: &[&str] = &["\"talosctl\"", "Command::new("];

    #[test]
    fn no_talosctl_invocations_outside_client_module() {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/talos");
        let entries = fs::read_dir(&dir).expect("read src/talos");

        let mut violations = Vec::new();
        for entry in entries {
            let entry = entry.expect("dir entry");
            let path = entry.path();
            if path.extension() != Some(OsStr::new("rs")) {
                continue;
            }
            if path.file_name() == Some(OsStr::new("client.rs")) {
                continue;
            }

            let content = fs::read_to_string(&path).expect("read source file");
            for (idx, line) in content.lines().enumerate() {
                let trimmed = line.trim_start();
                if trimmed.starts_with("//") {
                    continue;
                }
                for token in FORBIDDEN_TOKENS {
                    if line.contains(token) {
                        violations.push(format!(
                            "{}:{}: forbidden token `{}` outside client.rs",
                            path.display(),
                            idx + 1,
                            token
                        ));
                    }
                }
            }
        }

        assert!(
            violations.is_empty(),
            "talosctl invocations / Command::new must be confined to src/talos/client.rs:\n{}",
            violations.join("\n")
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
