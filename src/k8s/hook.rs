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
