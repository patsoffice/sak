//! Docker domain — read-only operations against a live Docker daemon.
//!
//! All commands talk to the daemon over its unix domain socket via the Docker
//! Engine REST API. The entire domain is gated behind the `docker` cargo
//! feature so that lean builds of sak are unaffected.
//!
//! # Read-only enforcement
//!
//! `hyper` exposes mutation methods (`POST`, `PUT`, `PATCH`, `DELETE`) on the
//! same client used for reads. To keep the domain provably read-only, **all**
//! HTTP access is confined to [`client`]. Other modules in `src/docker/` must
//! not import `hyper::Client`, `hyperlocal::*`, or construct
//! `Request::builder()` directly. A unit test in [`client`] enforces this by
//! grep, mirroring the `src/lxc/` and `src/k8s/` patterns.
//!
//! # Async bridge
//!
//! The rest of sak is synchronous. [`run`] builds a current-thread tokio
//! runtime locally and `block_on`s the async dispatcher, so adding `docker`
//! does not turn the rest of the binary async.

pub mod client;

use std::process::ExitCode;

use anyhow::Result;
use clap::Subcommand;

/// Subcommands of `sak docker`.
///
/// Currently empty — this is the foundation issue. Dependent issues
/// (`list`, `info`, `config`, `images`) populate it.
#[derive(Subcommand)]
pub enum DockerCommand {}

/// Dispatch a `sak docker` subcommand.
///
/// Builds a current-thread tokio runtime locally and `block_on`s the async
/// command body. The runtime is dropped before this function returns, so the
/// rest of sak stays sync.
pub fn run(cmd: &DockerCommand) -> Result<ExitCode> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    rt.block_on(async { dispatch(cmd).await })
}

async fn dispatch(cmd: &DockerCommand) -> Result<ExitCode> {
    // The enum is currently uninhabited — this match is exhaustive with no
    // arms. Dependent issues add the real arms.
    match *cmd {}
}
