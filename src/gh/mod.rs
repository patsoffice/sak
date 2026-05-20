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
pub mod issue_list;
pub mod pr_list;
pub mod release_list;
pub mod render;
pub mod run_list;
pub mod workflow_list;

use std::process::ExitCode;

use anyhow::Result;
use clap::Subcommand;

#[derive(Subcommand)]
pub enum GhCommand {
    Api(api::ApiArgs),
    PrList(pr_list::PrListArgs),
    IssueList(issue_list::IssueListArgs),
    RunList(run_list::RunListArgs),
    ReleaseList(release_list::ReleaseListArgs),
    WorkflowList(workflow_list::WorkflowListArgs),
}

pub fn run(cmd: &GhCommand) -> Result<ExitCode> {
    match cmd {
        GhCommand::Api(args) => api::run(args),
        GhCommand::PrList(args) => pr_list::run(args),
        GhCommand::IssueList(args) => issue_list::run(args),
        GhCommand::RunList(args) => run_list::run(args),
        GhCommand::ReleaseList(args) => release_list::run(args),
        GhCommand::WorkflowList(args) => workflow_list::run(args),
    }
}
