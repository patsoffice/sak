//! Agent-hook redirect rules for the `talos` domain.
//!
//! `talosctl get` and `talosctl read` map to `sak talos get`/`sak talos read`,
//! which fan out across every node in the active talosconfig. Both are
//! flattened to their own row with the subverb baked into the static message
//! (the registry takes `&'static str`, not a formatted string — same
//! flattening as the `*sum` tools in `hash`).
//!
//! Like the nix/gh/helm siblings, this file is exempt from
//! `src/talos/client.rs`'s binary-name chokepoint test: `tool: "talosctl"`
//! and the redirect messages mention `"talosctl"` legitimately. The
//! `Command::new(` half of that chokepoint still applies — hook rules never
//! spawn subprocesses.

use crate::hook::rule::HookRule;

pub const HOOK_RULES: &[HookRule] = &[
    HookRule {
        tool: "talosctl",
        subcommand: &[&["get"]],
        guard: None,
        message: "Use `sak talos get` instead of `talosctl get` \
             (fans out across nodes; also `sak talos certs` for fleet cert inventory).",
    },
    HookRule {
        tool: "talosctl",
        subcommand: &[&["read"]],
        guard: None,
        message: "Use `sak talos read` instead of `talosctl read` \
             (fans out across nodes; also `sak talos certs` for fleet cert inventory).",
    },
];
