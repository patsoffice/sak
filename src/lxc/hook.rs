//! Agent-hook redirect rules for the `lxc` domain.
//!
//! Two shell tools — `lxc` and `incus` (the open-source fork) — share the
//! same surface and both redirect to the same `sak lxc *` commands. Each
//! verb gets one row per tool with the tool name baked into the static
//! message (the registry takes `&'static str`, not a formatted string —
//! drops the check_lxc format! interpolation, same flattening pattern as
//! `hash::*sum`, `talos::hook`).
//!
//! Four read shapes per tool:
//! - bare `list` → `sak lxc list`
//! - `info` → `sak lxc info`
//! - `config show` → `sak lxc config`
//! - `image list|ls` → `sak lxc images`
//!
//! All other verbs (`launch`, `start`, `stop`, `exec`, `storage list`,
//! `network list`, ...) are writes or unshadowed reads and pass through.
//!
//! `#[cfg(feature = "lxc")]` gated in [`crate::hook::claude_code`]'s
//! `registries()` — a `--no-default-features` build drops these rules.

use crate::hook::rule::HookRule;

pub const HOOK_RULES: &[HookRule] = &[
    // ── lxc ─────────────────────────────────────────────
    HookRule {
        tool: "lxc",
        subcommand: &[&["list"]],
        guard: None,
        message: "Use `sak lxc list` instead of `lxc list`.",
    },
    HookRule {
        tool: "lxc",
        subcommand: &[&["info"]],
        guard: None,
        message: "Use `sak lxc info <instance>` instead of `lxc info`.",
    },
    HookRule {
        tool: "lxc",
        subcommand: &[&["config", "show"]],
        guard: None,
        message: "Use `sak lxc config <instance>` instead of `lxc config show`.",
    },
    HookRule {
        tool: "lxc",
        subcommand: &[&["image", "list"], &["image", "ls"]],
        guard: None,
        message: "Use `sak lxc images` instead of `lxc image list`.",
    },
    // ── incus ───────────────────────────────────────────
    HookRule {
        tool: "incus",
        subcommand: &[&["list"]],
        guard: None,
        message: "Use `sak lxc list` instead of `incus list`.",
    },
    HookRule {
        tool: "incus",
        subcommand: &[&["info"]],
        guard: None,
        message: "Use `sak lxc info <instance>` instead of `incus info`.",
    },
    HookRule {
        tool: "incus",
        subcommand: &[&["config", "show"]],
        guard: None,
        message: "Use `sak lxc config <instance>` instead of `incus config show`.",
    },
    HookRule {
        tool: "incus",
        subcommand: &[&["image", "list"], &["image", "ls"]],
        guard: None,
        message: "Use `sak lxc images` instead of `incus image list`.",
    },
];
