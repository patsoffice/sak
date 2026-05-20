//! Sole chokepoint for invoking the `gh` (GitHub CLI) binary.
//!
//! Every other module under `src/gh/` must call into the helpers exposed
//! here. Constructing a `std::process::Command` for `gh` from anywhere
//! else in the domain is forbidden, and the
//! [`tests::no_gh_invocations_outside_client_module`] grep test enforces
//! it on every `cargo test` run.
//!
//! Read-only enforcement is convention plus a noun/verb allowlist plus
//! the grep test. `gh` has a wide mutation surface (`pr create`,
//! `pr merge`, `issue close`, `repo create`, `workflow run`,
//! `secret set`, ...) so the chokepoint refuses any (noun, verb) pair
//! not on [`READ_ONLY_VERBS`]. There is no read-only flavor of `gh`
//! itself, so the allowlist is the cheapest credible defense.
//!
//! The `gh api` escape hatch — the catch-all REST/GraphQL caller — gets
//! a separate guard: it defaults to GET, but `-X / --method <verb>` can
//! switch to any HTTP method. We accept the noun `api` only when no
//! method flag is present, or when the method value is GET
//! (case-insensitive). Bare `gh api <endpoint>` is GET by `gh`'s own
//! default; `gh api … -X GET` is accepted as redundant-but-correct.

use std::process::Command;

use anyhow::{Context, Result, bail};

/// (noun, Option<verb>) pairs that `sak gh` is allowed to invoke.
///
/// When the verb slot is `Some("…")`, only that exact noun-verb pair is
/// allowed (e.g. `("pr", Some("list"))` permits `gh pr list` but not
/// `gh pr merge`). When the verb slot is `None`, every subcommand of
/// that noun is allowed — used for `search` (all `search …` forms are
/// reads) and `api` (further constrained by the per-method guard below).
///
/// Adding a new entry is a deliberate change: every pair here must be
/// strictly read-only against the remote (no PR/issue/release/repo
/// mutations, no workflow triggers, no auth state changes). Re-check
/// the verb's `gh <noun> <verb> --help` output before extending the
/// list.
pub const READ_ONLY_VERBS: &[(&str, Option<&str>)] = &[
    ("pr", Some("list")),
    ("pr", Some("view")),
    ("pr", Some("status")),
    ("pr", Some("diff")),
    ("pr", Some("checks")),
    ("issue", Some("list")),
    ("issue", Some("view")),
    ("issue", Some("status")),
    ("run", Some("list")),
    ("run", Some("view")),
    ("release", Some("list")),
    ("release", Some("view")),
    ("repo", Some("list")),
    ("repo", Some("view")),
    ("workflow", Some("list")),
    ("workflow", Some("view")),
    ("search", None),
    ("status", None),
    ("auth", Some("status")),
    ("api", None),
];

/// Output of one `gh` invocation. Stdout is bytes (so binary responses
/// — e.g. `gh api` returning a tarball — round-trip cleanly) and stderr
/// is text (so error reporting is readable).
#[derive(Debug)]
pub struct Output {
    pub stdout: Vec<u8>,
    pub stderr: String,
    pub status: std::process::ExitStatus,
}

/// Invoke `gh <noun> [<verb>] <args...>`.
///
/// The (noun, verb) pair must be permitted by [`READ_ONLY_VERBS`];
/// otherwise this returns an error without spawning a subprocess.
///
/// When `noun == "api"`, `args` is additionally scanned for `-X` /
/// `--method` flags; any value other than `GET` (case-insensitive) is
/// rejected. Bare `gh api <endpoint>` (no method flag) is GET by `gh`'s
/// own default and is accepted.
pub fn invoke(noun: &str, verb: Option<&str>, args: &[&str]) -> Result<Output> {
    if !verb_allowed(noun, verb) {
        bail!(
            "gh {} is not in the read-only allowlist ({})",
            display_pair(noun, verb),
            format_allowlist()
        );
    }

    if noun == "api" {
        check_api_method(args)?;
    }

    let mut cmd = Command::new("gh");
    cmd.arg(noun);
    if let Some(v) = verb {
        cmd.arg(v);
    }
    for a in args {
        cmd.arg(a);
    }

    let output = cmd
        .output()
        .with_context(|| "spawning `gh` (is it installed and on PATH?)".to_string())?;

    Ok(Output {
        stdout: output.stdout,
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        status: output.status,
    })
}

/// Convenience: run `invoke` and return stdout if the process exited 0.
/// On non-zero exit, surface stderr (trimmed) as an `anyhow::Error` —
/// callers that need finer-grained handling (e.g. distinguishing "404"
/// from "auth expired") should inspect `Output.stderr` themselves via
/// [`invoke`].
pub fn invoke_ok(noun: &str, verb: Option<&str>, args: &[&str]) -> Result<Vec<u8>> {
    let out = invoke(noun, verb, args)?;
    if !out.status.success() {
        let trimmed = out.stderr.trim();
        let suffix = if trimmed.is_empty() {
            String::new()
        } else {
            format!(": {}", trimmed)
        };
        bail!("gh {} failed{}", display_pair(noun, verb), suffix);
    }
    Ok(out.stdout)
}

fn verb_allowed(noun: &str, verb: Option<&str>) -> bool {
    READ_ONLY_VERBS.iter().any(|(n, v)| {
        *n == noun
            && match (v, verb) {
                (None, _) => true,
                (Some(allowed), Some(got)) => *allowed == got,
                (Some(_), None) => false,
            }
    })
}

fn display_pair(noun: &str, verb: Option<&str>) -> String {
    match verb {
        Some(v) => format!("`{} {}`", noun, v),
        None => format!("`{}`", noun),
    }
}

fn format_allowlist() -> String {
    let mut out = Vec::with_capacity(READ_ONLY_VERBS.len());
    for (n, v) in READ_ONLY_VERBS {
        out.push(match v {
            Some(v) => format!("{} {}", n, v),
            None => format!("{} *", n),
        });
    }
    out.join(", ")
}

/// Reject any `gh api` invocation whose `-X` / `--method` value is not
/// `GET` (case-insensitive). Both flag spellings accept either `-X GET`
/// (separate arg) or `-XGET` / `--method=GET` (joined). Bare `gh api
/// <endpoint>` is permitted because `gh`'s own default is GET.
fn check_api_method(args: &[&str]) -> Result<()> {
    let mut i = 0;
    while i < args.len() {
        let arg = args[i];

        let method = if arg == "-X" || arg == "--method" {
            i += 1;
            if i >= args.len() {
                bail!("gh api: `{}` requires a method value", arg);
            }
            Some(args[i])
        } else {
            arg.strip_prefix("-X")
                .or_else(|| arg.strip_prefix("--method="))
        };

        if let Some(m) = method
            && !m.eq_ignore_ascii_case("GET")
        {
            bail!(
                "gh api: HTTP method `{}` is not allowed — sak gh is read-only, only GET is permitted",
                m
            );
        }

        i += 1;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsStr;
    use std::fs;
    use std::path::PathBuf;

    /// Tokens that must not appear in any `src/gh/*.rs` file other than
    /// `client.rs`. Comments are exempt — the skip logic below ignores
    /// any line whose first non-whitespace characters are `//`.
    ///
    /// Two strings cover the surface: the literal binary name as a
    /// quoted string (`"gh"`) and the `Command::new(` constructor.
    /// Either alone would leave loopholes — you could spawn `gh` by
    /// building the `Command` from a variable, or build a non-`gh`
    /// `Command::new(...)` and then later re-target it. Together every
    /// realistic shell-out path trips the test.
    ///
    /// Note: `"gh"` is a short literal that could plausibly appear as
    /// substring of other strings (e.g. `"high"`, `"weight"`). The
    /// `contains` check below is intentionally substring-based — that
    /// trades a tiny false-positive risk for catching every `"gh"`
    /// usage including subtle ones (concatenation, `format!("{}", "gh")`,
    /// etc.). If a legitimate string in another `gh/*.rs` ever needs to
    /// contain the substring `"gh"` (e.g. in `long_about` documentation),
    /// move it to a `const` declared in `client.rs` and re-export it.
    const FORBIDDEN_TOKENS: &[&str] = &["\"gh\"", "Command::new("];

    #[test]
    fn no_gh_invocations_outside_client_module() {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/gh");
        let entries = fs::read_dir(&dir).expect("read src/gh");

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
            "gh invocations / Command::new must be confined to src/gh/client.rs:\n{}",
            violations.join("\n")
        );
    }

    #[test]
    fn rejects_mutating_verb() {
        for (noun, verb) in [
            ("pr", Some("merge")),
            ("issue", Some("close")),
            ("repo", Some("create")),
            ("workflow", Some("run")),
            ("auth", Some("login")),
            ("secret", Some("set")),
        ] {
            let err = invoke(noun, verb, &[]).unwrap_err();
            let msg = err.to_string();
            assert!(
                msg.contains("not in the read-only allowlist"),
                "expected allowlist rejection for `gh {} {:?}`, got: {}",
                noun,
                verb,
                msg
            );
        }
    }

    #[test]
    fn rejects_unknown_verb_under_allowed_noun() {
        // `pr` is on the allowlist but only with specific verbs.
        // `pr merge` is not one of them.
        let err = invoke("pr", Some("merge"), &[]).unwrap_err();
        assert!(err.to_string().contains("not in the read-only allowlist"));
    }

    #[test]
    fn rejects_non_get_api_method() {
        for args in [
            &["repos/x/y", "-X", "POST"][..],
            &["repos/x/y", "--method", "delete"][..],
            &["repos/x/y", "-XPATCH"][..],
            &["repos/x/y", "--method=PUT"][..],
        ] {
            let err = invoke("api", None, args).unwrap_err();
            let msg = err.to_string();
            assert!(
                msg.contains("not allowed") && msg.contains("GET"),
                "expected method rejection for args `{:?}`, got: {}",
                args,
                msg
            );
        }
    }

    #[test]
    fn rejects_dangling_method_flag() {
        let err = invoke("api", None, &["repos/x/y", "-X"]).unwrap_err();
        assert!(err.to_string().contains("requires a method value"));

        let err = invoke("api", None, &["repos/x/y", "--method"]).unwrap_err();
        assert!(err.to_string().contains("requires a method value"));
    }

    #[test]
    fn accepts_explicit_get() {
        // We can't easily assert the spawn succeeds without `gh` on
        // PATH in CI — but we can assert the *guards* don't reject the
        // call. Each form below should pass the allowlist + method
        // checks; the only way they'd error before spawning is a guard
        // failure, and the spawn error message is distinct from the
        // allowlist / method error messages, so we just check that the
        // returned error (if any) is *not* a guard rejection.
        for args in [
            &["repos/x/y", "-X", "GET"][..],
            &["repos/x/y", "--method", "get"][..],
            &["repos/x/y", "-XGET"][..],
            &["repos/x/y", "--method=GET"][..],
            &["repos/x/y"][..], // bare — default GET
        ] {
            match invoke("api", None, args) {
                Ok(_) => {}
                Err(e) => {
                    let msg = e.to_string();
                    assert!(
                        !msg.contains("not in the read-only allowlist")
                            && !msg.contains("not allowed"),
                        "guards rejected an explicit-GET form `{:?}`: {}",
                        args,
                        msg
                    );
                }
            }
        }
    }

    #[test]
    fn allows_verbless_noun() {
        // `search` and `status` are noun-only in the allowlist.
        // We can't assert spawn, but we can confirm the allowlist check
        // accepts a verbless invocation.
        // Reuse the same guard-vs-spawn discrimination as
        // accepts_explicit_get.
        for (noun, verb) in [("search", Some("code")), ("status", None)] {
            match invoke(noun, verb, &[]) {
                Ok(_) => {}
                Err(e) => {
                    let msg = e.to_string();
                    assert!(
                        !msg.contains("not in the read-only allowlist"),
                        "allowlist rejected `gh {} {:?}`: {}",
                        noun,
                        verb,
                        msg
                    );
                }
            }
        }
    }
}
