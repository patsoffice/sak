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
//! This module is the dispatch scaffold; individual commands (`list`, `get`,
//! `status`, `history`, `show`, `template`, `search`, `lint`, `repo-list`,
//! `dependency-list`) land as their own child issues and wire themselves into
//! [`HelmCommand`] as they arrive.

// The chokepoint's read-only enforcement is fully exercised by its own tests,
// but its spawn helpers (`invoke_ok`, the `Output` accessors) have no caller
// until the first command lands (sak-llm-fd6 and siblings wire into
// `HelmCommand`). Suppress dead-code warnings for the foundation; remove this
// once a command consumes `client::invoke_ok` / `Conn`.
#[allow(dead_code)]
pub mod client;

use std::process::ExitCode;

use anyhow::Result;
use clap::Subcommand;

#[derive(Subcommand)]
pub enum HelmCommand {}

pub fn run(cmd: &HelmCommand) -> Result<ExitCode> {
    match *cmd {}
}
