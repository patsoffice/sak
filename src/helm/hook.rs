//! Agent-hook redirect rules for the `helm` domain.
//!
//! `helm` is a verb-and-subverb CLI with a small handful of read aliases
//! (`helm ls` ≡ `helm list`, `helm hist` ≡ `helm history`, `helm inspect` ≡
//! `helm show`, `helm dep`/`dependencies list` ≡ `helm dependency list`).
//! Every alias collapses into the same row via the `subcommand` alternatives
//! slot. None of the rules need a guard — every (verb, subverb) pair listed
//! here is unambiguously a read; mutating verbs (`install`, `upgrade`,
//! `uninstall`, `rollback`, `repo add`/`update`/`remove`, `dependency
//! update`/`build`, ...) are absent from the table and pass through.
//!
//! Like the nix and gh siblings, this file is exempt from
//! `src/helm/client.rs`'s binary-name chokepoint test: `tool: "helm"` and the
//! redirect messages mention `"helm"` legitimately. The `Command::new(` half
//! of that chokepoint still applies — hook rules never spawn subprocesses.

use crate::hook::rule::HookRule;

pub const HOOK_RULES: &[HookRule] = &[
    HookRule {
        tool: "helm",
        subcommand: &[&["list"], &["ls"]],
        guard: None,
        message: "Use `sak helm list` instead of `helm list`/`helm ls` (TSV/JSON, --status/--filter/-A).",
    },
    HookRule {
        tool: "helm",
        subcommand: &[&["status"]],
        guard: None,
        message: "Use `sak helm status <release>` instead of `helm status` (TSV/JSON, --revision).",
    },
    HookRule {
        tool: "helm",
        subcommand: &[&["get"]],
        guard: None,
        message: "Use `sak helm get <release> --what all|manifest|values|notes|hooks` instead of `helm get`.",
    },
    // `helm inspect` is the deprecated alias for `helm show`.
    HookRule {
        tool: "helm",
        subcommand: &[&["show"], &["inspect"]],
        guard: None,
        message: "Use `sak helm show <chart> --what all|chart|values|readme|crds` instead of `helm show`.",
    },
    // `helm template` renders locally and never contacts the cluster.
    HookRule {
        tool: "helm",
        subcommand: &[&["template"]],
        guard: None,
        message: "Use `sak helm template <chart>` instead of `helm template` (offline render to YAML).",
    },
    HookRule {
        tool: "helm",
        subcommand: &[&["lint"]],
        guard: None,
        message: "Use `sak helm lint <chart>` instead of `helm lint` (TSV findings + pass/fail).",
    },
    // Both `helm search repo` and `helm search hub` are reads.
    HookRule {
        tool: "helm",
        subcommand: &[&["search"]],
        guard: None,
        message: "Use `sak helm search <term> --source repo|hub` instead of `helm search`.",
    },
    HookRule {
        tool: "helm",
        subcommand: &[&["history"], &["hist"]],
        guard: None,
        message: "Use `sak helm history <release>` instead of `helm history` (TSV/JSON, --max).",
    },
    // Only `repo list` is a read; `repo add`/`update`/`remove` are writes
    // sak can't perform, so they pass through (the table has no rule for them).
    HookRule {
        tool: "helm",
        subcommand: &[&["repo", "list"]],
        guard: None,
        message: "Use `sak helm repo-list` instead of `helm repo list` (TSV/JSON).",
    },
    // `dependency` aliases: `dep`, `dependencies`. Only `list` is a read;
    // `dependency update`/`build` are writes (they fetch + write Chart.lock).
    HookRule {
        tool: "helm",
        subcommand: &[
            &["dependency", "list"],
            &["dependencies", "list"],
            &["dep", "list"],
        ],
        guard: None,
        message: "Use `sak helm dependency-list <chart>` instead of `helm dependency list` (TSV/JSON).",
    },
];
