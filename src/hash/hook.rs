//! Agent-hook redirect rules for the `hash` domain.
//!
//! `openssl dgst` (the second half of the `openssl` split — the `x509` half
//! lives in [`crate::cert::hook`]) plus the five GNU/coreutils `*sum` /
//! `b3sum` digest tools. Every `*sum` invocation is a read (or a `--check`
//! verify, which maps to `sak hash --verify`), so they have no guards —
//! every call redirects.
//!
//! Each `*sum` tool is flattened into its own row with the matching `sak hash`
//! algo and the tool name baked into the static message (the registry takes
//! `&'static str`, not a formatted string — same flattening pattern used for
//! `yq`/`tomlq` and `rg`/`ripgrep`). `shasum` defaults to SHA-1 but commonly
//! takes `-a 256`; the legacy `check_sum` mapped it to `sha256`, so we do the
//! same here.

use crate::hook::rule::HookRule;

pub const HOOK_RULES: &[HookRule] = &[
    HookRule {
        tool: "openssl",
        subcommand: &[&["dgst"]],
        guard: None,
        message: "Use `sak hash sha256|sha1|md5|blake3 <file>` instead of `openssl dgst`.",
    },
    HookRule {
        tool: "sha256sum",
        subcommand: &[],
        guard: None,
        message: "Use `sak hash sha256 <file>` instead of `sha256sum` \
             (add `--verify <sumfile>` to check; other algos: sha256, sha1, md5, blake3).",
    },
    HookRule {
        tool: "sha1sum",
        subcommand: &[],
        guard: None,
        message: "Use `sak hash sha1 <file>` instead of `sha1sum` \
             (add `--verify <sumfile>` to check; other algos: sha256, sha1, md5, blake3).",
    },
    HookRule {
        tool: "md5sum",
        subcommand: &[],
        guard: None,
        message: "Use `sak hash md5 <file>` instead of `md5sum` \
             (add `--verify <sumfile>` to check; other algos: sha256, sha1, md5, blake3).",
    },
    HookRule {
        tool: "shasum",
        subcommand: &[],
        guard: None,
        message: "Use `sak hash sha256 <file>` instead of `shasum` \
             (add `--verify <sumfile>` to check; other algos: sha256, sha1, md5, blake3).",
    },
    HookRule {
        tool: "b3sum",
        subcommand: &[],
        guard: None,
        message: "Use `sak hash blake3 <file>` instead of `b3sum` \
             (add `--verify <sumfile>` to check; other algos: sha256, sha1, md5, blake3).",
    },
];
