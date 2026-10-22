//! Shared subprocess spawner for the shell-out domains (`talos`, `gh`,
//! `helm`, `nix`).
//!
//! Those four chokepoints grew the same shape independently: an `Output`
//! struct (bytes stdout, text stderr, exit status), a spawn helper that maps
//! spawn failures to "is it installed and on PATH?", an `invoke_ok` shape
//! that surfaces trimmed stderr on a non-zero exit, and a `(verb,
//! Option<subverb>)` allowlist matcher. They live here once. What stays
//! per-domain, deliberately:
//!
//! - the verb tables themselves (each binary's read surface is different),
//! - the argv assembly (connection flags, experimental-features injection,
//!   `eval --read-only`, env passthrough),
//! - the ok-message rendering wherever a domain needs more than the shared
//!   shape (`talos`'s node suffix, `gh`'s concise stderr; `helm` uses the
//!   shared [`LocalBinClient::run_ok`] verbatim),
//! - extra mappings (`helm`'s not-found → `None`).
//!
//! Read-only enforcement: [`LocalBinClient::run`] is the only real
//! `Command::new(` in `src/` outside integration-test harnesses (`tests/`
//! legitimately spawns the built binary). The
//! [`tests::no_shell_spawns_outside_shared_chokepoint`] grep test enforces
//! this tree-wide; the four domain `client.rs` files are exempt there only
//! for their test token literal, and each domain keeps its own binary-name
//! test (no direct shell-out naming its binary outside its
//! `client.rs`/`hook.rs`) plus a domain-local `Command::new(` absence test,
//! so a new direct spawn trips at least two tests.

use std::ffi::OsStr;
use std::process::{Command, ExitStatus};

use anyhow::{Context, Result, bail};

/// Output of one child invocation. Stdout is bytes (so binary payloads —
/// `talosctl read`, `helm get` output — round-trip cleanly) and stderr is
/// text (so error reporting is readable).
#[derive(Debug)]
pub struct Output {
    pub stdout: Vec<u8>,
    pub stderr: String,
    pub status: ExitStatus,
}

/// A named binary this domain is allowed to spawn. One value per binary
/// (`talosctl`, `gh`, `helm`, `nix`, `nix-store`); the binary name rides in
/// error messages so failures identify which tool was missing or failed.
pub struct LocalBinClient {
    bin: &'static str,
}

impl LocalBinClient {
    /// Name the binary this client spawns. `const` so domains can hold a
    /// single shared value rather than constructing per call.
    pub const fn new(bin: &'static str) -> Self {
        Self { bin }
    }

    /// Spawn `<bin> <argv...>` and collect the output. Spawn failures (binary
    /// missing, not executable) become an error naming the binary; a
    /// non-zero exit is *not* an error here — callers decide via
    /// [`Output::status`] (see [`LocalBinClient::run_ok`]).
    pub fn run<S: AsRef<OsStr>>(&self, argv: &[S]) -> Result<Output> {
        let mut cmd = Command::new(self.bin);
        for a in argv {
            cmd.arg(a);
        }

        let output = cmd
            .output()
            .with_context(|| format!("spawning `{}` (is it installed and on PATH?)", self.bin))?;

        Ok(Output {
            stdout: output.stdout,
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
            status: output.status,
        })
    }

    /// Spawn `<bin> <argv...>` with extra environment variables for the
    /// child. Same as [`run`](Self::run) otherwise. Only `gh` uses this
    /// today (`GH_REPO` for `{owner}`/`{repo}` placeholders); anything that
    /// changes auth or the target host stays in the caller's own environment.
    pub fn run_with_env<S: AsRef<OsStr>>(
        &self,
        argv: &[S],
        env: &[(&str, &str)],
    ) -> Result<Output> {
        let mut cmd = Command::new(self.bin);
        for a in argv {
            cmd.arg(a);
        }
        cmd.envs(env.iter().copied());

        let output = cmd
            .output()
            .with_context(|| format!("spawning `{}` (is it installed and on PATH?)", self.bin))?;

        Ok(Output {
            stdout: output.stdout,
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
            status: output.status,
        })
    }

    /// Spawn `<bin> <argv...>` and return stdout if the process exited 0. On
    /// non-zero exit, surface stderr (trimmed) as an `anyhow::Error` shaped
    /// as `<bin> <what> failed[: stderr]`.
    pub fn run_ok<S: AsRef<OsStr>>(&self, what: &str, argv: &[S]) -> Result<Vec<u8>> {
        let out = self.run(argv)?;
        if !out.status.success() {
            bail!("{} {what} failed{}", self.bin, stderr_suffix(&out.stderr));
        }
        Ok(out.stdout)
    }
}

/// The `: <trimmed stderr>` suffix shared by every non-zero-exit error.
/// Empty stderr contributes nothing, so a bare failure with no diagnostics
/// reads as `<bin> <what> failed` with no trailing colon.
pub fn stderr_suffix(stderr: &str) -> String {
    let trimmed = stderr.trim();
    if trimmed.is_empty() {
        String::new()
    } else {
        format!(": {trimmed}")
    }
}

/// Whether `(verb, subverb)` is covered by a `(verb, Option<subverb>)`
/// allowlist table. A `None` entry for `verb` admits any subverb (the
/// subverb is treated as a read-only argument); otherwise the provided
/// subverb must equal a `Some(_)` entry exactly. Shared by the `gh`, `helm`,
/// and `nix` tables (`talos` uses a plain verb list and a `contains`
/// check); each domain keeps its own message rendering.
pub fn pair_allowed(table: &[(&str, Option<&str>)], verb: &str, subverb: Option<&str>) -> bool {
    table.iter().any(|(v, sv)| {
        *v == verb
            && match (sv, subverb) {
                (None, _) => true,
                (Some(want), Some(got)) => *want == got,
                (Some(_), None) => false,
            }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `Command::new(` must appear nowhere under `src/` except here: every
    /// shell-out domain routes spawning through [`LocalBinClient`].
    /// Integration tests under `tests/` legitimately spawn the built binary
    /// and shell snippets, so the scan is confined to `src/`. The four domain
    /// `client.rs` files are exempt: each holds only its domain test's token
    /// literal (the string the grep looks for), not a spawn — and none of
    /// them imports `std::process::Command` anymore, so a smuggled spawn
    /// would need an import line no reviewer would miss.
    #[test]
    fn no_shell_spawns_outside_shared_chokepoint() {
        crate::test_support::assert_no_forbidden_tokens_outside(
            &[
                "local_bin.rs",
                "talos/client.rs",
                "gh/client.rs",
                "helm/client.rs",
                "nix/client.rs",
            ],
            &["Command::new("],
            "subprocess spawning must be confined to src/local_bin.rs (LocalBinClient)",
        );
    }

    #[test]
    fn stderr_suffix_empty_stays_bare() {
        assert_eq!(stderr_suffix(""), "");
        assert_eq!(stderr_suffix("  \n "), "");
    }

    #[test]
    fn stderr_suffix_trims_and_colons() {
        assert_eq!(stderr_suffix("boom\n"), ": boom");
        assert_eq!(stderr_suffix("  multi\nline  "), ": multi\nline");
    }

    #[test]
    fn pair_allowed_whole_family_admits_any_subverb() {
        let table = &[("get", None)];
        assert!(pair_allowed(table, "get", Some("manifest")));
        assert!(pair_allowed(table, "get", None));
    }

    #[test]
    fn pair_allowed_pinned_subverb_is_exact() {
        let table = &[("repo", Some("list"))];
        assert!(pair_allowed(table, "repo", Some("list")));
        assert!(!pair_allowed(table, "repo", Some("add")));
        assert!(!pair_allowed(table, "repo", None));
        assert!(!pair_allowed(table, "install", None));
    }
}
