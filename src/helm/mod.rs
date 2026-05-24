//! `helm` domain — read-only inspection of Helm releases, charts, and repos.
//!
//! Sibling to the [`crate::k8s`] domain: LLMs reach for `helm` constantly in
//! Kubernetes incident triage (what release state is this in? what was
//! actually rendered? what values are live?), and today that path bottoms out
//! at shelling to `helm` directly. This domain formalizes the read-only
//! subset behind a verb allowlist.
//!
//! Like [`crate::talos`] and the nix domain, `sak helm` shells out to the
//! system `helm` binary rather than re-implementing the release client; the
//! cost is one external runtime dependency in exchange for not tracking
//! Helm's storage backend and chart-rendering surface. If `helm` isn't on
//! PATH, the chokepoint returns a clear error.
//!
//! Read-only enforcement lives in [`client`]: a `(verb, subverb)` allowlist
//! refuses every mutating subcommand (`install`, `upgrade`, `uninstall`,
//! `repo add`, ...), and a grep test forbids `Command::new` / `"helm"`
//! outside `client.rs`.
//!
//! Individual commands (`get`, `status`, `history`, `show`, `template`,
//! `search`, `lint`, `repo-list`, `dependency-list`) land as their own child
//! issues and wire themselves into [`HelmCommand`] as they arrive.

pub mod client;
pub mod list;

use std::process::ExitCode;

use anyhow::Result;
use clap::Subcommand;

#[derive(Subcommand)]
pub enum HelmCommand {
    List(list::ListArgs),
}

pub fn run(cmd: &HelmCommand) -> Result<ExitCode> {
    match cmd {
        HelmCommand::List(args) => list::run(args),
    }
}
