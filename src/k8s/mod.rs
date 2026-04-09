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
pub mod containers;
pub mod contexts;
pub mod describe;
pub mod discovery;
pub mod env;
pub mod events;
pub mod failing;
pub mod get;
pub mod images;
pub mod kinds;
pub mod pending;
pub mod restarts;
pub mod schema;

use std::process::ExitCode;

use anyhow::Result;
use clap::Subcommand;

/// Subcommands of `sak k8s`.
#[derive(Subcommand)]
pub enum K8sCommand {
    /// List every context in the merged kubeconfig (no apiserver call)
    Contexts(contexts::ContextsArgs),
    /// List every group/version/kind exposed by the cluster
    Kinds(kinds::KindsArgs),
    /// List or get resources of a kind
    Get(get::GetArgs),
    /// List container images across resources
    Images(images::ImagesArgs),
    /// List env vars on a single pod-bearing resource
    Env(env::EnvArgs),
    /// Fetch the OpenAPI v3 schema for a kind
    Schema(schema::SchemaArgs),
    /// List pod containers with restarts
    Restarts(restarts::RestartsArgs),
    /// List pods that are not Running or Succeeded
    Failing(failing::FailingArgs),
    /// List pods stuck in Pending
    Pending(pending::PendingArgs),
    /// List cluster events, newest first
    Events(events::EventsArgs),
    /// Aggregated description of one resource (object/status/containers/owners/events)
    Describe(describe::DescribeArgs),
}

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
    match cmd {
        K8sCommand::Contexts(args) => contexts::run(args).await,
        K8sCommand::Kinds(args) => kinds::run(args).await,
        K8sCommand::Get(args) => get::run(args).await,
        K8sCommand::Images(args) => images::run(args).await,
        K8sCommand::Env(args) => env::run(args).await,
        K8sCommand::Schema(args) => schema::run(args).await,
        K8sCommand::Restarts(args) => restarts::run(args).await,
        K8sCommand::Failing(args) => failing::run(args).await,
        K8sCommand::Pending(args) => pending::run(args).await,
        K8sCommand::Events(args) => events::run(args).await,
        K8sCommand::Describe(args) => describe::run(args).await,
    }
}
