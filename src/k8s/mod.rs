//! Kubernetes domain — read-only operations against a live cluster.
//!
//! All commands talk to the cluster via the `kube` crate using the user's
//! kubeconfig (or in-cluster service account). The entire domain is gated
//! behind the `k8s` cargo feature so that default builds of sak are unaffected
//! in size and compile time.
//!
//! # Read-only enforcement
//!
//! `kube::Api` exposes mutation methods (`create`, `delete`, `patch`, ...) on
//! the same type used for reads. To keep the domain provably read-only, **all**
//! `kube::Api` usage is confined to [`client`]. Other modules in `src/k8s/`
//! must not import `kube::Api` or any of its mutation methods. A unit test in
//! [`client`] enforces this by grep.
//!
//! # Async bridge
//!
//! The rest of sak is synchronous. [`run`] builds a current-thread tokio
//! runtime locally and `block_on`s the async dispatcher, so adding `k8s` does
//! not turn the rest of the binary async.

pub mod client;
pub mod discovery;

use std::process::ExitCode;

use anyhow::Result;
use clap::Subcommand;

/// Subcommands of `sak k8s`.
///
/// This enum is intentionally empty in the foundation issue (sak-llm-k7e).
/// Subsequent issues add `Kinds`/`Get` (sak-llm-ovb), `Images`/`Env`
/// (sak-llm-m78), and `Schema` (sak-llm-ium).
#[derive(Subcommand)]
pub enum K8sCommand {}

/// Dispatch a `sak k8s` subcommand.
///
/// Builds a current-thread tokio runtime locally and `block_on`s the async
/// command body. The runtime is dropped before this function returns, so the
/// rest of sak stays sync.
pub fn run(cmd: &K8sCommand) -> Result<ExitCode> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    rt.block_on(async { dispatch(cmd).await })
}

async fn dispatch(cmd: &K8sCommand) -> Result<ExitCode> {
    match *cmd {}
}
