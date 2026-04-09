//! LXC domain — read-only operations against a live LXD or Incus daemon.
//!
//! All commands talk to the daemon over its unix domain socket via the LXD
//! REST API (which Incus also speaks). The entire domain is gated behind the
//! `lxc` cargo feature so that lean builds of sak are unaffected.
//!
//! # Read-only enforcement
//!
//! `hyper` exposes mutation methods (`POST`, `PUT`, `PATCH`, `DELETE`) on the
//! same client used for reads. To keep the domain provably read-only, **all**
//! HTTP access is confined to [`client`]. Other modules in `src/lxc/` must not
//! import `hyper::Client`, `hyperlocal::*`, or construct `Request::builder()`
//! directly. A unit test in [`client`] enforces this by grep.
//!
//! # Async bridge
//!
//! The rest of sak is synchronous. [`run`] builds a current-thread tokio
//! runtime locally and `block_on`s the async dispatcher, so adding `lxc` does
//! not turn the rest of the binary async.

pub mod client;

use std::process::ExitCode;

use anyhow::Result;
use clap::Subcommand;

/// Subcommands of `sak lxc`.
///
/// Currently empty — this is the foundation issue. Dependent issues
/// (`list`, `info`, `config`, `images`) populate it.
#[derive(Subcommand)]
pub enum LxcCommand {}

/// Dispatch a `sak lxc` subcommand.
///
/// Builds a current-thread tokio runtime locally and `block_on`s the async
/// command body. The runtime is dropped before this function returns, so the
/// rest of sak stays sync.
pub fn run(cmd: &LxcCommand) -> Result<ExitCode> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    rt.block_on(async { dispatch(cmd).await })
}

async fn dispatch(cmd: &LxcCommand) -> Result<ExitCode> {
    // The enum is currently uninhabited — this match is exhaustive with no
    // arms. Dependent issues add the real arms.
    match *cmd {}
}
