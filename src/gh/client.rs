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
/// When `noun == "api"`, `args` is additionally classified by
/// [`api_method`]: any explicit non-GET `-X` / `--method`, and any request
/// parameters (`-f`/`-F`/`--input`) on a REST endpoint without an explicit
/// `-X GET` (gh would switch to POST), are rejected.
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

/// The HTTP method a `gh api` invocation will use. Shared by the
/// [`check_api_method`] enforcement below and the hook guard
/// (`gh_api_method_is_get` in `src/gh/hook.rs`) so the redirect decision and
/// the enforcement can never disagree about what counts as a read.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ApiMethod {
    /// A REST GET: no method flag and no request parameters (gh's default),
    /// or an explicit `-X GET` (which sends any parameters as a query string).
    Get,
    /// The `graphql` endpoint whose every `query` document defines only
    /// `query` operations and fragments. gh always POSTs GraphQL, so the
    /// document — not the HTTP method — is what makes it a read.
    GraphQl,
    /// A GraphQL document defining a non-`query` operation (`mutation`,
    /// `subscription`, ...), named here.
    GraphQlWrite(String),
    /// A GraphQL request whose document sak can't inspect (a body from
    /// `--input`, a query read from `@file`, a malformed document) — the
    /// reason is named here. Rejected rather than trusted.
    GraphQlUnverifiable(String),
    /// First explicit non-GET method seen.
    NonGet(String),
    /// Request parameters or a body (`-f`/`-F`/`--input`, named here) on a
    /// REST endpoint without an explicit GET: gh silently switches to POST.
    ImplicitPost(String),
    /// Value-taking flag with no following value.
    Dangling(String),
}

/// The endpoint gh treats as GraphQL (always POSTed by gh).
pub(crate) const GRAPHQL_ENDPOINT: &str = "graphql";

/// `gh api` flags that consume a value, as (short, long). Needed to tell
/// flag values from the endpoint positional, and to find the value of a
/// short flag inside a pflag cluster such as `-iXPOST`.
const API_VALUE_FLAGS: &[(Option<char>, &str)] = &[
    (Some('X'), "method"),
    (Some('H'), "header"),
    (Some('F'), "field"),
    (Some('f'), "raw-field"),
    (Some('q'), "jq"),
    (Some('t'), "template"),
    (Some('p'), "preview"),
    (None, "input"),
    (None, "cache"),
    (None, "hostname"),
];

/// Flags that add request parameters or a body, so gh POSTs unless told GET.
const API_BODY_FLAGS: &[&str] = &["field", "raw-field", "input"];

/// Classify a `gh api` argv (with or without the leading `api` token).
///
/// Parses the way gh's pflag does: `-X GET`, `-XGET`, `-X=GET`,
/// `--method GET`, `--method=GET`, and short clusters where a boolean
/// (`-i`) precedes a value flag (`-iXPOST`, `-iX POST`). Every method flag
/// is checked rather than stopping at the first: `-X GET -X POST` is a POST
/// (last wins in gh), and any non-GET is rejected outright.
pub(crate) fn api_method<S: AsRef<str>>(args: &[S]) -> ApiMethod {
    let mut explicit_get = false;
    let mut body_flag: Option<String> = None;
    let mut graphql = false;
    // GraphQL documents sent as the `query` field, and the first reason (if
    // any) a document can't be inspected.
    let mut query_docs: Vec<String> = Vec::new();
    let mut unverifiable: Option<String> = None;

    // Record one parsed (long name, value) flag; returns early on a non-GET.
    let mut on_flag = |long: &str, value: &str, spelled: &str| -> Option<ApiMethod> {
        if long == "method" {
            if !value.eq_ignore_ascii_case("GET") {
                return Some(ApiMethod::NonGet(value.to_string()));
            }
            explicit_get = true;
            return None;
        }
        if API_BODY_FLAGS.contains(&long) && body_flag.is_none() {
            body_flag = Some(spelled.to_string());
        }
        if long == "input" && unverifiable.is_none() {
            unverifiable = Some(format!("a request body from `{spelled}`"));
        }
        if (long == "field" || long == "raw-field")
            && let Some(("query", doc)) = value.split_once('=')
        {
            // `-F` (typed) reads `@path` / `@-` from a file or stdin; `-f`
            // (raw) sends the text verbatim.
            if long == "field" && doc.starts_with('@') {
                if unverifiable.is_none() {
                    unverifiable = Some(format!("a query read from a file, `query={doc}`"));
                }
            } else {
                query_docs.push(doc.to_string());
            }
        }
        None
    };

    let mut i = 0;
    let mut positional_only = false;
    while i < args.len() {
        let arg = args[i].as_ref();
        i += 1;

        if positional_only || arg == "-" || !arg.starts_with('-') {
            if arg == GRAPHQL_ENDPOINT {
                graphql = true;
            }
            continue;
        }
        if arg == "--" {
            positional_only = true;
            continue;
        }

        if let Some(long_arg) = arg.strip_prefix("--") {
            let (name, inline) = match long_arg.split_once('=') {
                Some((n, v)) => (n, Some(v)),
                None => (long_arg, None),
            };
            if !API_VALUE_FLAGS.iter().any(|(_, l)| *l == name) {
                continue; // boolean long flag (--paginate, --include, ...)
            }
            let value = match inline {
                Some(v) => v,
                None => match args.get(i) {
                    Some(v) => {
                        i += 1;
                        v.as_ref()
                    }
                    None => return ApiMethod::Dangling(arg.to_string()),
                },
            };
            if let Some(m) = on_flag(name, value, &format!("--{name}")) {
                return m;
            }
            continue;
        }

        // Short cluster: booleans until the first value flag, which takes the
        // rest of the token (minus a leading `=`) or the next argument.
        let cluster = &arg[1..];
        for (pos, c) in cluster.char_indices() {
            let Some((_, long)) = API_VALUE_FLAGS.iter().find(|(s, _)| *s == Some(c)) else {
                continue;
            };
            let rest = &cluster[pos + c.len_utf8()..];
            let value = if !rest.is_empty() {
                rest.strip_prefix('=').unwrap_or(rest)
            } else {
                match args.get(i) {
                    Some(v) => {
                        i += 1;
                        v.as_ref()
                    }
                    None => return ApiMethod::Dangling(format!("-{c}")),
                }
            };
            if let Some(m) = on_flag(long, value, &format!("-{c}")) {
                return m;
            }
            break;
        }
    }

    if graphql {
        // The document decides, whatever the method: inspect every `query`
        // field gh will send (gh's own pick among duplicates doesn't matter
        // when all of them must pass).
        if let Some(reason) = unverifiable {
            return ApiMethod::GraphQlUnverifiable(reason);
        }
        for doc in &query_docs {
            match graphql_operation_types(doc) {
                Err(reason) => return ApiMethod::GraphQlUnverifiable(reason.to_string()),
                Ok(ops) => {
                    if let Some(op) = ops.iter().find(|op| !matches!(**op, "query" | "fragment")) {
                        return ApiMethod::GraphQlWrite((*op).to_string());
                    }
                }
            }
        }
        return if explicit_get {
            ApiMethod::Get
        } else {
            ApiMethod::GraphQl
        };
    }

    if explicit_get {
        ApiMethod::Get
    } else if let Some(flag) = body_flag {
        ApiMethod::ImplicitPost(flag)
    } else {
        ApiMethod::Get
    }
}

/// The keyword opening each top-level definition of a GraphQL executable
/// document: `query`, `mutation`, `subscription`, `fragment`, or `query`
/// for the anonymous `{ … }` shorthand. Every definition is reported, not
/// just the first — `query A {…} mutation B {…}` with `operationName=B`
/// runs the mutation.
///
/// A deliberately small scanner, not a parser: it skips comments, strings
/// and block strings, ignores braces inside parentheses (object values in
/// arguments and variable defaults), and takes the first name at brace depth
/// 0 after each definition closes. That position is exactly where GraphQL
/// puts the operation keyword, so an operation *named* `mutation` doesn't
/// confuse it. Unterminated strings and unbalanced brackets are errors: a
/// document sak can't scan is not one it can vouch for.
fn graphql_operation_types(doc: &str) -> std::result::Result<Vec<&str>, &'static str> {
    let b = doc.as_bytes();
    let mut ops = Vec::new();
    let (mut depth, mut parens) = (0usize, 0usize);
    let mut expect_definition = true;
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'#' => {
                while i < b.len() && b[i] != b'\n' {
                    i += 1;
                }
            }
            b'"' if b[i..].starts_with(b"\"\"\"") => {
                i += 3;
                loop {
                    if i >= b.len() {
                        return Err("an unterminated block string in the GraphQL document");
                    }
                    if b[i..].starts_with(b"\\\"\"\"") {
                        i += 4;
                    } else if b[i..].starts_with(b"\"\"\"") {
                        i += 3;
                        break;
                    } else {
                        i += 1;
                    }
                }
                continue;
            }
            b'"' => {
                i += 1;
                loop {
                    match b.get(i) {
                        None | Some(b'\n') => {
                            return Err("an unterminated string in the GraphQL document");
                        }
                        Some(b'\\') => i += 2,
                        Some(b'"') => break,
                        Some(_) => i += 1,
                    }
                }
            }
            b'(' => parens += 1,
            b')' => {
                parens = parens
                    .checked_sub(1)
                    .ok_or("unbalanced parentheses in the GraphQL document")?;
            }
            b'{' if parens == 0 => {
                if depth == 0 && expect_definition {
                    ops.push("query");
                    expect_definition = false;
                }
                depth += 1;
            }
            b'}' if parens == 0 => {
                depth = depth
                    .checked_sub(1)
                    .ok_or("unbalanced braces in the GraphQL document")?;
                if depth == 0 {
                    expect_definition = true;
                }
            }
            c if c == b'_' || c.is_ascii_alphabetic() => {
                let start = i;
                while i < b.len() && (b[i] == b'_' || b[i].is_ascii_alphanumeric()) {
                    i += 1;
                }
                if depth == 0 && parens == 0 && expect_definition {
                    ops.push(&doc[start..i]);
                    expect_definition = false;
                }
                continue;
            }
            _ => {}
        }
        i += 1;
    }
    if depth != 0 || parens != 0 {
        return Err("unbalanced brackets in the GraphQL document");
    }
    Ok(ops)
}

/// Reject any `gh api` invocation that would not be a read: an explicit
/// non-GET method, REST request parameters without `-X GET` (gh's implicit
/// POST), or a GraphQL document that isn't provably query-only. Bare
/// `gh api <endpoint>` is GET by gh's own default.
fn check_api_method(args: &[&str]) -> Result<()> {
    match api_method(args) {
        ApiMethod::Get | ApiMethod::GraphQl => Ok(()),
        ApiMethod::GraphQlWrite(op) => bail!(
            "gh api graphql: `{}` operations are not allowed — sak gh is read-only, only \
             `query` documents are permitted",
            op
        ),
        ApiMethod::GraphQlUnverifiable(reason) => bail!(
            "gh api graphql: can't verify the request is read-only ({}); pass the query \
             inline with `--raw-field query='…'`",
            reason
        ),
        ApiMethod::NonGet(m) => bail!(
            "gh api: HTTP method `{}` is not allowed — sak gh is read-only, only GET is permitted",
            m
        ),
        ApiMethod::ImplicitPost(flag) => bail!(
            "gh api: `{}` adds request parameters, which makes gh send a POST — sak gh is \
             read-only; add `-X GET` to send them as a query string",
            flag
        ),
        ApiMethod::Dangling(flag) => bail!("gh api: `{}` requires a value", flag),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Binary-name string: its only legitimate appearance outside
    /// `client.rs` is as the `tool:` field of a [`crate::hook::rule::HookRule`]
    /// in `src/gh/hook.rs` and inside that file's static redirect messages
    /// (which mention "gh" by design), so that file is exempt.
    ///
    /// Note: `"gh"` is a short literal that could plausibly appear as
    /// substring of other strings (e.g. `"high"`, `"weight"`). The
    /// `contains` check below is intentionally substring-based — that
    /// trades a tiny false-positive risk for catching every `"gh"`
    /// usage including subtle ones (concatenation, `format!("{}", "gh")`,
    /// etc.). If a legitimate string in another `gh/*.rs` ever needs to
    /// contain the substring `"gh"` (e.g. in `long_about` documentation),
    /// move it to a `const` declared in `client.rs` and re-export it.
    const GH_NAME_TOKEN: &[&str] = &["\"gh\""];

    /// The `Command::new(` constructor — banned strictly (no `hook.rs`
    /// exemption) because hook rules are pure data and never spawn
    /// subprocesses. Banning the name alone would leave the variable-built
    /// `Command` loophole; banning the constructor alone would let a
    /// non-`gh` `Command` be re-targeted. The two assertions together close
    /// every realistic shell-out path.
    const COMMAND_NEW_TOKEN: &[&str] = &["Command::new("];

    #[test]
    fn no_gh_name_token_outside_client_or_hook() {
        crate::test_support::assert_no_forbidden_tokens_except(
            "gh",
            GH_NAME_TOKEN,
            &["client.rs", "hook.rs"],
            "the \"gh\" name literal must be confined to client.rs (chokepoint) \
             or hook.rs (HookRule.tool fields + redirect messages)",
        );
    }

    #[test]
    fn no_command_new_outside_client_module() {
        crate::test_support::assert_no_forbidden_tokens(
            "gh",
            COMMAND_NEW_TOKEN,
            "Command::new(...) must be confined to src/gh/client.rs",
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
    fn rejects_mixed_method_flags() {
        // `gh api x -X GET -X POST` is a POST (last wins in gh): the shared
        // scanner must reject it even though the first flag says GET.
        for args in [
            &["repos/x/y", "-X", "GET", "-X", "POST"][..],
            &["repos/x/y", "-X", "POST", "-X", "GET"][..],
        ] {
            let err = invoke("api", None, args).unwrap_err();
            assert!(
                err.to_string().contains("not allowed"),
                "expected method rejection for args `{:?}`, got: {}",
                args,
                err
            );
        }
    }

    #[test]
    fn rejects_dangling_method_flag() {
        let err = invoke("api", None, &["repos/x/y", "-X"]).unwrap_err();
        assert!(err.to_string().contains("`-X` requires a value"), "{err}");

        let err = invoke("api", None, &["repos/x/y", "--method"]).unwrap_err();
        assert!(
            err.to_string().contains("`--method` requires a value"),
            "{err}"
        );
    }

    #[test]
    fn rejects_rest_params_without_explicit_get() {
        // Regression (sak-llm-16cp): gh switches to POST when parameters are
        // added, so `-f title=x` against a REST endpoint is a write. Rejected
        // before any subprocess is spawned.
        for args in [
            &["repos/x/y/issues", "-f", "title=x"][..],
            &["repos/x/y/issues", "-F", "n=1"][..],
            &["repos/x/y/issues", "--raw-field", "title=x"][..],
            &["repos/x/y/issues", "--field=n=1"][..],
            &["repos/x/y/issues", "-ftitle=x"][..],
            &["repos/x/y/rulesets", "--input", "body.json"][..],
        ] {
            let err = invoke("api", None, args).unwrap_err();
            assert!(
                err.to_string().contains("makes gh send a POST"),
                "expected implicit-POST rejection for `{:?}`, got: {}",
                args,
                err
            );
        }
    }

    #[test]
    fn api_method_params_follow_the_effective_method() {
        // Explicit GET turns parameters into a query string: still a read.
        assert_eq!(
            api_method(&["search/issues", "-X", "GET", "-f", "q=is:open"]),
            ApiMethod::Get
        );
        assert_eq!(
            api_method(&["-f", "q=x", "search/issues", "--method=get"]),
            ApiMethod::Get
        );
        // Without it, gh POSTs.
        assert_eq!(
            api_method(&["repos/x/y/issues", "-f", "title=x"]),
            ApiMethod::ImplicitPost("-f".into())
        );
        // A flag value that happens to look like a body flag is not one.
        assert_eq!(
            api_method(&["repos/x/y", "--jq", "--input"]),
            ApiMethod::Get
        );
    }

    #[test]
    fn api_method_graphql_is_its_own_class() {
        // gh always POSTs GraphQL; query documents are reads.
        for args in [
            &["graphql", "-f", "query={ viewer { login } }"][..],
            &[
                "api",
                "graphql",
                "-F",
                "n=1",
                "-f",
                "query=query($n: Int) { a(n: $n) }",
            ][..],
        ] {
            assert_eq!(api_method(args), ApiMethod::GraphQl, "{args:?}");
        }
        // `graphql` as a flag value is not the endpoint.
        assert_eq!(
            api_method(&["repos/x/y", "-H", "graphql", "-f", "a=b"]),
            ApiMethod::ImplicitPost("-f".into())
        );
    }

    #[test]
    fn graphql_operation_types_reads_every_definition() {
        let ops = |d| graphql_operation_types(d).unwrap();
        assert_eq!(ops("{ viewer { login } }"), ["query"]);
        assert_eq!(ops("query Q($n: Int) { a(n: $n) { b } }"), ["query"]);
        assert_eq!(
            ops("query { ...F } fragment F on T { a }"),
            ["query", "fragment"]
        );
        assert_eq!(ops("mutation { addStar(input: {}) { x } }"), ["mutation"]);
        assert_eq!(ops("subscription S { e }"), ["subscription"]);
        // The mutation hides behind a harmless first operation.
        assert_eq!(ops("query A { a } mutation B { b }"), ["query", "mutation"]);
        assert_eq!(ops(""), Vec::<&str>::new());
    }

    #[test]
    fn graphql_operation_types_ignores_keywords_outside_definition_position() {
        let ops = |d| graphql_operation_types(d).unwrap();
        // An operation *named* mutation, a field named mutation.
        assert_eq!(ops("query mutation { mutation }"), ["query"]);
        // Keywords and braces inside strings, block strings and comments.
        assert_eq!(
            ops("query { a(s: \"} mutation {\") }\n# } mutation { x }\n"),
            ["query"]
        );
        assert_eq!(
            ops("query { a(s: \"\"\"} \\\"\"\" mutation {\"\"\") }"),
            ["query"]
        );
        // Object values in variable defaults and arguments.
        assert_eq!(
            ops("query ($f: In = {a: {b: 1}}) { x(f: {c: 2}) }"),
            ["query"]
        );
    }

    #[test]
    fn graphql_operation_types_rejects_what_it_cannot_scan() {
        for doc in [
            "query { a(s: \"unterminated) }",
            "query { a(s: \"\"\"open) }",
            "query { a ",
            "query { a } }",
            "query ( { a }",
        ] {
            assert!(graphql_operation_types(doc).is_err(), "{doc:?}");
        }
    }

    #[test]
    fn api_method_classifies_graphql_documents() {
        // Regression (sak-llm-a08h): mutations ride the same POST as queries.
        assert_eq!(
            api_method(&["graphql", "-f", "query=mutation { addStar { x } }"]),
            ApiMethod::GraphQlWrite("mutation".into())
        );
        assert_eq!(
            api_method(&[
                "graphql",
                "-f",
                "query=query A { a } mutation B { b }",
                "-f",
                "operationName=B"
            ]),
            ApiMethod::GraphQlWrite("mutation".into())
        );
        // Every `query` field must pass, not just the last.
        assert_eq!(
            api_method(&[
                "graphql",
                "--raw-field=query=mutation { m }",
                "-f",
                "query={ viewer { login } }"
            ]),
            ApiMethod::GraphQlWrite("mutation".into())
        );
        // An explicit GET doesn't excuse the document.
        assert_eq!(
            api_method(&["graphql", "-X", "GET", "-f", "query=mutation { m }"]),
            ApiMethod::GraphQlWrite("mutation".into())
        );
        // Variables are not documents.
        assert_eq!(
            api_method(&["graphql", "-F", "owner=mutation", "-f", "query={ a }"]),
            ApiMethod::GraphQl
        );
    }

    #[test]
    fn api_method_rejects_uninspectable_graphql_bodies() {
        for args in [
            &["graphql", "--input", "body.json"][..],
            &["graphql", "-F", "query=@q.graphql"][..],
            &["graphql", "--field", "query=@-"][..],
            &["graphql", "-f", "query={ a(s: \"open) }"][..],
        ] {
            assert!(
                matches!(api_method(args), ApiMethod::GraphQlUnverifiable(_)),
                "{args:?}"
            );
        }
    }

    #[test]
    fn rejects_graphql_writes_before_spawning() {
        let err = invoke("api", None, &["graphql", "-f", "query=mutation { m }"]).unwrap_err();
        assert!(
            err.to_string()
                .contains("`mutation` operations are not allowed"),
            "{err}"
        );
        let err = invoke("api", None, &["graphql", "--input", "b.json"]).unwrap_err();
        assert!(err.to_string().contains("can't verify"), "{err}");
    }

    #[test]
    fn api_method_parses_short_flag_clusters() {
        // pflag clusters: a boolean (-i) then a value flag that takes the
        // rest of the token or the next argument.
        assert_eq!(
            api_method(&["x", "-iXPOST"]),
            ApiMethod::NonGet("POST".into())
        );
        assert_eq!(
            api_method(&["x", "-iX", "DELETE"]),
            ApiMethod::NonGet("DELETE".into())
        );
        assert_eq!(api_method(&["x", "-X=GET"]), ApiMethod::Get);
        assert_eq!(api_method(&["x", "-iXGET"]), ApiMethod::Get);
        assert_eq!(
            api_method(&["x", "-if", "a=b"]),
            ApiMethod::ImplicitPost("-f".into())
        );
        assert_eq!(api_method(&["x", "-i"]), ApiMethod::Get);
        assert_eq!(api_method(&["x", "-iX"]), ApiMethod::Dangling("-X".into()));
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
