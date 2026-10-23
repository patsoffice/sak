//! Chokepoint for invoking the `nix` binary.
//!
//! Every other module under `src/nix/` must call into the helpers exposed
//! here. Spawning itself is the shared [`crate::local_bin::LocalBinClient`]
//! (the tree-wide grep test confines all `Command::new(` to it); naming the
//! `nix` binaries directly from anywhere else in the domain is forbidden,
//! and the [`tests::no_nix_name_tokens_outside_client_or_hook`] grep test
//! enforces that on every `cargo test` run.
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

use std::ffi::OsString;

use anyhow::{Result, bail};

use crate::local_bin::{LocalBinClient, pair_allowed, stderr_suffix};

pub use crate::local_bin::Output;

/// (verb, subverb) pairs of `nix` that this domain is allowed to invoke.
/// Anything not covered here is rejected at the chokepoint with a hard error —
/// no fallthrough, no env-var override.
///
/// A `None` subverb means the *whole verb family* is read-only — e.g.
/// `nix path-info` and `nix eval` take their arguments directly with no
/// read/write subverb split. A `Some(sv)` entry locks the verb to that one
/// read-only subverb: `nix flake show` is allowed but `nix flake update`
/// is not, because `flake` has no `None` entry.
///
/// Adding a new entry is a deliberate change: every verb here must be strictly
/// read-only (no builds, no store writes, no profile/registry mutations, no
/// flake.lock rewrites). Re-check the verb's `nix <verb> --help` output before
/// extending the list.
pub const READ_ONLY_VERBS: &[(&str, Option<&str>)] = &[
    ("flake", Some("show")),
    ("flake", Some("metadata")),
    ("path-info", None),
    ("derivation", Some("show")),
    ("profile", Some("list")),
    ("registry", Some("list")),
    ("eval", None),
    ("store", Some("info")),
];

/// The single `nix` spawner. All subprocesses go through the shared
/// [`LocalBinClient`]; the `"nix"` literal lives here (and in `hook.rs`
/// tool names) by chokepoint-test exemption.
static NIX: LocalBinClient = LocalBinClient::new("nix");

/// The single `nix-store` spawner. A second binary (the classic stable CLI),
/// so it gets its own client value rather than reusing [`NIX`].
static NIX_STORE: LocalBinClient = LocalBinClient::new("nix-store");

/// Invoke `nix --extra-experimental-features '…' <verb> [subverb] <args...>`.
///
/// The `(verb, subverb)` pair must be covered by [`READ_ONLY_VERBS`];
/// otherwise this returns an error without spawning a subprocess.
///
/// `nix-command flakes` experimental features are injected unconditionally so
/// the domain works on stock nix. For `eval`, `--read-only` is injected too so
/// expression evaluation can't write to the store.
pub fn invoke(verb: &str, subverb: Option<&str>, args: &[&str]) -> Result<Output> {
    ensure_allowed(verb, subverb)?;
    NIX.run(&build_argv(verb, subverb, args))
}

/// Convenience: run [`invoke`] and return stdout if the process exited 0. On
/// non-zero exit, surface stderr (trimmed) as an `anyhow::Error` — same shape
/// as the shared `run_ok`, routed through [`invoke`] so the raw `Output`
/// stays available to callers that need it.
pub fn invoke_ok(verb: &str, subverb: Option<&str>, args: &[&str]) -> Result<Vec<u8>> {
    let out = invoke(verb, subverb, args)?;
    if !out.status.success() {
        bail!(
            "nix {} failed{}",
            display_verb(verb, subverb),
            stderr_suffix(&out.stderr)
        );
    }
    Ok(out.stdout)
}

/// Assemble the `nix --extra-experimental-features … <verb> [subverb]
/// [--read-only] <args...>` argv.
///
/// `nix-command flakes` experimental features are injected unconditionally so
/// the domain works on stock nix. For `eval`, `--read-only` is injected too
/// so expression evaluation can't write to the store.
fn build_argv(verb: &str, subverb: Option<&str>, args: &[&str]) -> Vec<OsString> {
    let mut argv: Vec<OsString> = vec![
        "--extra-experimental-features".into(),
        "nix-command flakes".into(),
        verb.into(),
    ];
    if let Some(sv) = subverb {
        argv.push(sv.into());
    }
    // `nix eval` evaluates arbitrary expressions; force read-only so it can't
    // realise derivations or otherwise write to the store.
    if verb == "eval" {
        argv.push("--read-only".into());
    }
    argv.extend(args.iter().map(OsString::from));
    argv
}

/// Reject a `(verb, subverb)` pair not covered by [`READ_ONLY_VERBS`] before
/// any subprocess spawns. Shared by [`invoke`] and [`invoke_ok`] so the
/// allowlist can't be bypassed by picking the wrong entry point
/// (`nix_store_query` carries its own flag check for the second binary).
fn ensure_allowed(verb: &str, subverb: Option<&str>) -> Result<()> {
    if !verb_allowed(verb, subverb) {
        bail!(
            "nix verb `{}` is not in the read-only allowlist ({})",
            display_verb(verb, subverb),
            allowlist_summary()
        );
    }
    Ok(())
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

/// `nix-store --query` sub-flags this domain is allowed to use.
///
/// `nix-store` is a *second*, separate (stable) binary from `nix`, shelled out
/// to only here. It has plenty of mutating operations (`--delete`, `--gc`,
/// `--add`, `--realise`, ...), so the chokepoint only ever runs `--query` and
/// only with one of these read-only sub-flags. Reverse dependencies
/// (`--referrers`) have no modern `nix` subcommand equivalent, which is why this
/// domain shells out to `nix-store` at all rather than reusing `nix path-info`.
pub const NIX_STORE_QUERY_FLAGS: &[&str] = &["--references", "--referrers", "--requisites"];

/// Invoke `nix-store --query <flag> <path>` and return stdout on success.
///
/// `flag` must be a member of [`NIX_STORE_QUERY_FLAGS`]; otherwise this returns
/// an error without spawning a subprocess. `nix-store` is the classic stable
/// CLI and needs no experimental-features flags. On non-zero exit, surface
/// stderr (trimmed) as an `anyhow::Error`.
pub fn nix_store_query(flag: &str, path: &str) -> Result<Vec<u8>> {
    if !NIX_STORE_QUERY_FLAGS.contains(&flag) {
        bail!(
            "nix-store --query `{}` is not in the read-only allowlist ({})",
            flag,
            NIX_STORE_QUERY_FLAGS.join(", ")
        );
    }

    NIX_STORE.run_ok(
        &format!("nix-store --query {flag}"),
        &["--query", flag, path],
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Binary-name strings: their only legitimate appearance outside
    /// `client.rs` is as the `tool:` field of a [`crate::hook::rule::HookRule`]
    /// in `src/nix/hook.rs`, so that file is exempt. Banning the names alone
    /// would leave the `Command::new(` loophole — see the next test.
    const NIX_NAME_TOKENS: &[&str] = &["\"nix\"", "\"nix-store\""];

    /// `Command::new(` must appear nowhere in this domain — not even in
    /// `client.rs`: all spawning goes through the shared `LocalBinClient`
    /// (see the tree-wide `no_shell_spawns_outside_shared_chokepoint`
    /// test). Banning the constructor alongside the binary names closes
    /// the variable-built `Command` loophole as well. The two assertions
    /// together close
    /// every realistic shell-out path for either binary.
    const COMMAND_NEW_TOKEN: &[&str] = &[crate::test_support::COMMAND_NEW];

    #[test]
    fn no_nix_name_tokens_outside_client_or_hook() {
        crate::test_support::assert_no_forbidden_tokens_except(
            "nix",
            NIX_NAME_TOKENS,
            &["client.rs", "hook.rs"],
            "nix/nix-store name literals must be confined to client.rs (chokepoint) \
             or hook.rs (HookRule.tool fields)",
        );
    }

    #[test]
    fn no_command_new_in_domain() {
        // No exemption, client.rs included: the token is built with
        // `concat!`, so this file doesn't match its own test.
        crate::test_support::assert_no_forbidden_tokens_except(
            "nix",
            COMMAND_NEW_TOKEN,
            &[],
            "no src/nix/ file may reference the Command constructor; \
             all spawning goes through the shared LocalBinClient",
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
        // Pinned-subverb verbs admit only their listed read subverbs.
        assert!(verb_allowed("flake", Some("show")));
        assert!(verb_allowed("flake", Some("metadata")));
        assert!(!verb_allowed("flake", Some("update")));
        assert!(!verb_allowed("flake", None));
        assert!(verb_allowed("store", Some("info")));
        assert!(!verb_allowed("store", Some("delete")));
        assert!(verb_allowed("profile", Some("list")));
        assert!(!verb_allowed("profile", Some("install")));
        // Unknown verbs are rejected outright.
        assert!(!verb_allowed("build", None));
    }

    #[test]
    fn rejects_allowlist_entries_without_commands() {
        // `flake info` (deprecated alias of `flake metadata`), `store ls`,
        // and `why-depends` were dropped from the allowlist: no sak command
        // stands behind them (sak-llm-5svf). Re-adding any of them must come
        // with the command, not silently.
        for (verb, subverb) in [
            ("flake", Some("info")),
            ("store", Some("ls")),
            ("why-depends", None),
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
    fn nix_store_rejects_unknown_query_flag() {
        // Only the three read-only ref queries are allowed; a mutating or
        // unlisted query flag is rejected before spawning nix-store.
        for flag in ["--delete", "--gc", "--outputs", "--deriver", "--tree"] {
            let err = nix_store_query(flag, "/nix/store/x").unwrap_err();
            assert!(
                err.to_string().contains("not in the read-only allowlist"),
                "{flag} should be rejected, got: {err}"
            );
        }
    }

    #[test]
    fn nix_store_admits_ref_query_flags() {
        for flag in NIX_STORE_QUERY_FLAGS {
            assert!(["--references", "--referrers", "--requisites"].contains(flag));
        }
    }
}
