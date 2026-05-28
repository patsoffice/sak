//! Agent-hook redirect rules for the `docker` domain.
//!
//! Three single-verb reads: `docker ps` (→ `sak docker list`), `docker
//! images` (→ `sak docker images`), and `docker inspect` (→ either
//! `sak docker info` or `sak docker config` depending on whether the
//! caller wants the runtime state or the spec). All other docker verbs are
//! writes or unshadowed reads and pass through.
//!
//! `#[cfg(feature = "docker")]` gated in [`crate::hook::claude_code`]'s
//! `registries()` — a `--no-default-features` build drops these rules.

use crate::hook::rule::HookRule;

pub const HOOK_RULES: &[HookRule] = &[
    HookRule {
        tool: "docker",
        subcommand: &[&["ps"]],
        guard: None,
        message: "Use `sak docker list` instead of `docker ps`.",
    },
    HookRule {
        tool: "docker",
        subcommand: &[&["images"]],
        guard: None,
        message: "Use `sak docker images` instead of `docker images`.",
    },
    HookRule {
        tool: "docker",
        subcommand: &[&["inspect"]],
        guard: None,
        message: "Use `sak docker info <container>` or `sak docker config <container>` \
             instead of `docker inspect`.",
    },
];
