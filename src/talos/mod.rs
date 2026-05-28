//! `talos` domain — read-only inspection of a Talos Linux cluster.
//!
//! Talos exposes a structured COSI resource model (`talosctl get`) and an
//! on-disk filesystem (`talosctl read`). This domain wraps both with two
//! pieces of value the raw `talosctl` invocation lacks:
//!
//! 1. **Fan-out across every node in the active talosconfig context** —
//!    most cluster-health triage reads the same path / resource off every
//!    control-plane node and stitches the results.
//! 2. **Read-only enforcement** — `talosctl` has plenty of mutating
//!    subcommands. The chokepoint in [`client`] refuses to invoke any verb
//!    not on its allowlist, and a grep test forbids `Command::new` /
//!    `"talosctl"` outside `client.rs`. Stronger than convention: even a
//!    well-meaning future contributor cannot accidentally land a path that
//!    pokes a mutating verb.
//!
//! `sak talos` shells out to the system `talosctl` binary rather than
//! re-implementing the COSI client; the cost of that decision is one
//! external runtime dependency in exchange for not having to track the
//! upstream gRPC schema. If `talosctl` isn't on PATH, the chokepoint
//! returns a clear error.
//!
//! See [`crate::cert`] for the X.509 parsing layer reused by `sak talos
//! certs`.

pub mod certs;
pub mod client;
pub mod config;
pub mod get;
pub mod hook;
pub mod read;

use crate::output::Outcome;

use anyhow::Result;
use clap::Subcommand;

#[derive(Subcommand)]
pub enum TalosCommand {
    Certs(certs::CertsArgs),
    Read(read::ReadArgs),
    Get(get::GetArgs),
}

pub fn run(cmd: &TalosCommand) -> Result<Outcome> {
    match cmd {
        TalosCommand::Certs(args) => certs::run(args),
        TalosCommand::Read(args) => read::run(args),
        TalosCommand::Get(args) => get::run(args),
    }
}
