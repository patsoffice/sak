//! Prometheus / Alertmanager domain — read-only operations over HTTP.
//!
//! All commands talk to the endpoint via the `ureq` blocking HTTP client over
//! TCP + TLS. The entire domain is gated behind the `prom` cargo feature so
//! lean builds of sak are unaffected.
//!
//! # Read-only enforcement
//!
//! `ureq` exposes mutation methods (`post`, `put`, `patch`, `delete`) on the
//! same agent used for reads. To keep the domain provably read-only, **all**
//! HTTP access is confined to [`client`]. Other modules in `src/prom/` must
//! not import `ureq::Agent`, call `ureq::agent(`, or use any non-GET method.
//! A unit test in [`client`] enforces this by grep.
//!
//! Prometheus exposes admin write endpoints under `/api/v1/admin/tsdb/*` when
//! the server is started with `--web.enable-admin-api`, so the chokepoint is
//! genuinely guarding something, not just decorative.
//!
//! # Sync, not async
//!
//! Unlike `k8s` / `lxc` / `docker`, the prom domain is fully synchronous —
//! `ureq` is a blocking client, and each command is one HTTP round trip.
//! Adding `prom` does not pull tokio into the binary.

pub mod alerts;
pub mod client;
pub mod duration;
pub mod histogram;
pub mod query;
pub mod range;

use std::process::ExitCode;

use anyhow::Result;
use clap::Subcommand;

/// Subcommands of `sak prom`. Subsequent commits add `targets`, `rules`,
/// and `am alerts | silences`.
#[derive(Subcommand)]
pub enum PromCommand {
    /// List alerts on a Prometheus server
    Alerts(alerts::AlertsArgs),
    /// Run an instant PromQL query
    Query(query::QueryArgs),
    /// Run a range PromQL query
    QueryRange(range::RangeArgs),
    /// Pretty-print a Prometheus histogram's buckets
    Histogram(histogram::HistogramArgs),
}

/// Dispatch a `sak prom` subcommand. Synchronous — no tokio runtime.
pub fn run(cmd: &PromCommand) -> Result<ExitCode> {
    match cmd {
        PromCommand::Alerts(args) => alerts::run(args),
        PromCommand::Query(args) => query::run(args),
        PromCommand::QueryRange(args) => range::run(args),
        PromCommand::Histogram(args) => histogram::run(args),
    }
}
