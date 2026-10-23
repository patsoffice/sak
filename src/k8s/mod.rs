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

pub mod children;
pub mod client;
pub mod containers;
pub mod contexts;
pub mod describe;
pub mod discovery;
pub mod endpoints;
pub mod env;
pub mod events;
pub mod failing;
pub mod get;
pub mod hook;
pub mod images;
pub mod kinds;
pub mod logs;
pub mod not_ready;
pub mod owners;
pub mod pending;
pub mod podtable;
pub mod restarts;
pub mod schema;
pub mod top;

use crate::output::Outcome;

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
    /// List objects whose target condition is not at the wanted status
    NotReady(not_ready::NotReadyArgs),
    /// List cluster events, newest first
    Events(events::EventsArgs),
    /// Aggregated description of one resource (object/status/containers/owners/events)
    Describe(describe::DescribeArgs),
    /// Backing endpoints of a Service
    Endpoints(endpoints::EndpointsArgs),
    /// Walk ownerReferences up to the root owner
    Owners(owners::OwnersArgs),
    /// List immediate children of a resource
    Children(children::ChildrenArgs),
    /// Fetch container logs from a pod
    Logs(logs::LogsArgs),
    /// CPU/memory usage from metrics-server
    Top(top::TopArgs),
}

/// Dispatch a `sak k8s` subcommand.
///
/// Delegates to [`crate::output::run_async`], which builds a current-thread
/// tokio runtime locally and `block_on`s the async command body. The runtime
/// is dropped before this function returns, so the rest of sak stays sync.
pub fn run(cmd: &K8sCommand) -> Result<Outcome> {
    crate::output::run_async(dispatch(cmd))
}

async fn dispatch(cmd: &K8sCommand) -> Result<Outcome> {
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
        K8sCommand::NotReady(args) => not_ready::run(args).await,
        K8sCommand::Events(args) => events::run(args).await,
        K8sCommand::Describe(args) => describe::run(args).await,
        K8sCommand::Endpoints(args) => endpoints::run(args).await,
        K8sCommand::Owners(args) => owners::run(args).await,
        K8sCommand::Children(args) => children::run(args).await,
        K8sCommand::Logs(args) => logs::run(args).await,
        K8sCommand::Top(args) => top::run(args).await,
    }
}
