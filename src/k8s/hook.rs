//! Agent-hook redirect rules for the `k8s` domain.
//!
//! Each `kubectl` read verb (`get`, `describe`, `logs`, `events`) is
//! flattened to its own row with the subverb baked into the static message
//! (drops the check_kubectl format! interpolation, same flattening pattern as
//! `hash::*sum`, `config::yq`, `talos::hook`). Three additional standalone
//! reads — `kubectl api-resources` (→ `sak k8s kinds`), `kubectl explain`
//! (→ `sak k8s schema`), and the two-verb `kubectl config get-contexts`
//! (→ `sak k8s contexts`) — round out the table.
//!
//! This file is `#[cfg(feature = "k8s")]` in [`crate::hook::claude_code`]'s
//! `registries()`. In a lean (`--no-default-features`) build the rules
//! disappear entirely so the binary doesn't suggest commands it doesn't
//! ship — the headline fix the registry mechanism unlocks.

use crate::hook::rule::HookRule;

/// Split `tokens` into positionals, accepting only the given boolean flags
/// and value flags (separated `-n x`, joined `--namespace=x`). Returns
/// `None` on any other flag or a value flag missing its value, so a guard
/// declines rather than redirect something sak would reject. `value_ok`
/// vets a value flag's value (e.g. `--sort-by` only takes cpu|memory).
fn scan_flags<'a>(
    tokens: &'a [String],
    bool_flags: &[&str],
    value_flags: &[&str],
    value_ok: impl Fn(&str, &str) -> bool,
) -> Option<Vec<&'a str>> {
    let mut positionals = Vec::new();
    let mut i = 0;
    while i < tokens.len() {
        let t = tokens[i].as_str();
        i += 1;
        if !t.starts_with('-') {
            positionals.push(t);
            continue;
        }
        if bool_flags.contains(&t) {
            continue;
        }
        let (flag, value) = match t.split_once('=') {
            Some((f, v)) => (f, v),
            None => {
                let v = tokens.get(i)?.as_str();
                i += 1;
                (t, v)
            }
        };
        if !value_flags.contains(&flag) || !value_ok(flag, value) {
            return None;
        }
    }
    Some(positionals)
}

/// `kubectl auth can-i <verb> <resource>` with only the flags `sak k8s
/// can-i` mirrors (`-n`, `-A`, `--subresource`). `--list`, `--as`,
/// `--as-group`, `-q`, extra positionals, and non-resource URLs (`/healthz`)
/// pass through to kubectl.
fn kubectl_can_i_is_supported(args: &[String]) -> bool {
    // args: ["auth", "can-i", ...]
    let Some(rest) = args.get(2..) else {
        return false;
    };
    let Some(positionals) = scan_flags(
        rest,
        &["-A", "--all-namespaces"],
        &["-n", "--namespace", "--subresource"],
        |_, _| true,
    ) else {
        return false;
    };
    positionals.len() == 2 && !positionals[1].starts_with('/')
}

/// `kubectl top pods|nodes` with only what `sak k8s top` serves: `pod`/`po`/
/// `node`/`no` spellings, `-n`/`-A`/`-l` for pods, `--sort-by cpu|memory`.
/// A named pod or node, `--containers`, `--no-headers`, and other flags pass
/// through to kubectl.
fn kubectl_top_is_supported(args: &[String]) -> bool {
    // args: ["top", <resource>, ...]
    let Some(kind) = args.get(1).map(String::as_str) else {
        return false;
    };
    let rest = &args[2..];
    let sort_ok =
        |flag: &str, value: &str| flag != "--sort-by" || matches!(value, "cpu" | "memory");
    let positionals = match kind {
        "pods" | "pod" | "po" => scan_flags(
            rest,
            &["-A", "--all-namespaces"],
            &["-n", "--namespace", "-l", "--selector", "--sort-by"],
            sort_ok,
        ),
        "nodes" | "node" | "no" => scan_flags(rest, &[], &["--sort-by"], sort_ok),
        _ => None,
    };
    positionals.is_some_and(|p| p.is_empty())
}

pub const HOOK_RULES: &[HookRule] = &[
    HookRule {
        tool: "kubectl",
        subcommand: &[&["get"]],
        guard: None,
        message: "Use `sak k8s get` instead of `kubectl get`. \
             Also: sak k8s failing/pending/restarts/images/contexts/kinds/schema.",
    },
    HookRule {
        tool: "kubectl",
        subcommand: &[&["describe"]],
        guard: None,
        message: "Use `sak k8s describe` instead of `kubectl describe`. \
             Also: sak k8s failing/pending/restarts/images/contexts/kinds/schema.",
    },
    HookRule {
        tool: "kubectl",
        subcommand: &[&["logs"]],
        guard: None,
        message: "Use `sak k8s logs` instead of `kubectl logs`. \
             Also: sak k8s failing/pending/restarts/images/contexts/kinds/schema.",
    },
    HookRule {
        tool: "kubectl",
        subcommand: &[&["events"]],
        guard: None,
        message: "Use `sak k8s events` instead of `kubectl events`. \
             Also: sak k8s failing/pending/restarts/images/contexts/kinds/schema.",
    },
    HookRule {
        tool: "kubectl",
        subcommand: &[&["api-resources"]],
        guard: None,
        message: "Use `sak k8s kinds` instead of `kubectl api-resources`.",
    },
    // `can-i` and `top` redirect only the forms the sak command serves;
    // `--list`, `--as`, a named pod, `--containers`, ... stay with kubectl.
    HookRule {
        tool: "kubectl",
        subcommand: &[&["auth", "can-i"]],
        guard: Some(kubectl_can_i_is_supported),
        message: "Use `sak k8s can-i <verb> <resource>` instead of `kubectl auth can-i` \
             (-n, -A, --subresource; same namespace default as kubectl).",
    },
    HookRule {
        tool: "kubectl",
        subcommand: &[&["top"]],
        guard: Some(kubectl_top_is_supported),
        message: "Use `sak k8s top pods|nodes` instead of `kubectl top` \
             (-n, -A, -l, --sort-by cpu|memory; sorted by CPU by default).",
    },
    HookRule {
        tool: "kubectl",
        subcommand: &[&["explain"]],
        guard: None,
        message: "Use `sak k8s schema <group/version/Kind>` instead of `kubectl explain`.",
    },
    HookRule {
        tool: "kubectl",
        subcommand: &[&["config", "get-contexts"]],
        guard: None,
        message: "Use `sak k8s contexts` instead of `kubectl config get-contexts`.",
    },
];

#[cfg(test)]
mod tests {
    use super::*;

    fn a(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn top_guard_matches_only_what_sak_serves() {
        for args in [
            &["top", "pods"][..],
            &["top", "pod", "-A"][..],
            &["top", "po", "-n", "kube-system", "-l", "app=x"][..],
            &["top", "pods", "--sort-by=memory"][..],
            &["top", "nodes", "--sort-by", "cpu"][..],
            &["top", "node"][..],
        ] {
            assert!(kubectl_top_is_supported(&a(args)), "{args:?}");
        }
        // Regression: these were redirected to a sak command that rejects them.
        for args in [
            &["top", "pod", "web-0"][..],
            &["top", "node", "k8s-00"][..],
            &["top", "pods", "--containers"][..],
            &["top", "pods", "--sort-by", "name"][..],
            &["top", "pods", "--no-headers"][..],
            &["top", "nodes", "-l", "role=worker"][..],
            &["top"][..],
            &["top", "services"][..],
        ] {
            assert!(!kubectl_top_is_supported(&a(args)), "{args:?}");
        }
    }

    #[test]
    fn can_i_guard_matches_only_what_sak_serves() {
        for args in [
            &["auth", "can-i", "list", "pods"][..],
            &["auth", "can-i", "get", "pods/web-0", "-n", "web"][..],
            &["auth", "can-i", "delete", "nodes", "-A"][..],
            &["auth", "can-i", "get", "pods", "--subresource=log"][..],
        ] {
            assert!(kubectl_can_i_is_supported(&a(args)), "{args:?}");
        }
        // Regression: these were redirected to a sak command that lacks them.
        for args in [
            &["auth", "can-i", "--list"][..],
            &[
                "auth",
                "can-i",
                "list",
                "pods",
                "--as",
                "system:serviceaccount:a:b",
            ][..],
            &["auth", "can-i", "list", "pods", "--as-group=dev"][..],
            &["auth", "can-i", "get", "/healthz"][..],
            &["auth", "can-i", "list", "pods", "-q"][..],
            &["auth", "can-i", "list"][..],
        ] {
            assert!(!kubectl_can_i_is_supported(&a(args)), "{args:?}");
        }
    }
}
