//! `gh` domain — read-only inspection via the GitHub CLI.
//!
//! This domain wraps a curated, read-only subset of `gh` (the GitHub
//! CLI) so LLM-driven triage of PRs, issues, workflow runs, releases,
//! and repos goes through sak's normal TSV/JSON output discipline
//! rather than shelling to `gh` ad-hoc or curling the REST/GraphQL
//! API.
//!
//! Mirrors the talos pattern: shell out to the system `gh` binary, no
//! Rust deps, no cargo feature. Read-only enforcement is convention
//! plus a noun/verb allowlist in [`client::READ_ONLY_VERBS`] plus a
//! grep test that forbids `Command::new` / `"gh"` literals outside
//! [`client`]. `gh` has a large mutation surface (`pr create / merge`,
//! `issue close`, `repo create`, `workflow run`, `secret set`,
//! `auth login`, ...), so the allowlist is strictly stronger than
//! convention — there is no read-only flavor of `gh` itself.
//!
//! The `gh api` escape hatch (`gh api <endpoint>` — the catch-all
//! REST/GraphQL caller) gets a per-method guard: invocations with
//! `-X` / `--method <verb>` are rejected unless the verb is `GET`
//! (case-insensitive). Bare `gh api <endpoint>` is `gh`'s own default
//! GET and is accepted.
//!
//! Auth is whatever `gh` itself uses — `~/.config/gh/hosts.yml` from
//! `gh auth login`, or `GH_TOKEN` / `GITHUB_TOKEN` from the
//! environment. sak passes the environment through unchanged; there is
//! no sak-side credential plumbing.

pub mod api;
pub mod client;

use std::process::ExitCode;

use anyhow::Result;
use clap::Subcommand;

#[derive(Subcommand)]
pub enum GhCommand {
    Api(api::ApiArgs),
}

pub fn run(cmd: &GhCommand) -> Result<ExitCode> {
    match cmd {
        GhCommand::Api(args) => api::run(args),
    }
}
