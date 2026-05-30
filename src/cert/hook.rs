//! Agent-hook redirect rules for the `cert` domain.
//!
//! `openssl` is shared between `cert` (the `x509` subcommand here) and `hash`
//! (the `dgst` subcommand, declared in [`crate::hash::hook`]). The two rules
//! have disjoint subcommands so they can't collide in the engine, and they sit
//! beside the commands they shadow rather than being clumped together.

use crate::hook::rule::HookRule;

pub const HOOK_RULES: &[HookRule] = &[HookRule {
    tool: "openssl",
    subcommand: &[&["x509"]],
    guard: None,
    message: "Use `sak cert inspect <cert>` instead of `openssl x509` \
         (omit <cert> to read PEM/DER from stdin, e.g. `cat cert.pem | sak cert inspect`). \
         Also: `sak cert expiring --days 30`, `sak cert from-kubeconfig`.",
}];
