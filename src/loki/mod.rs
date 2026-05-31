//! Grafana Loki domain — read-only LogQL operations over HTTP.
//!
//! Loki is the log-side counterpart to Prometheus on the metric side, so this
//! domain deliberately mirrors [`crate::prom`]: it talks to the endpoint via
//! the `ureq` blocking HTTP client over TCP + TLS, stays fully synchronous (no
//! tokio), and is gated behind the `loki` cargo feature. Because `prom` and
//! `loki` share the same `ureq` dependency, enabling both adds no crates over
//! enabling either alone.
//!
//! # Read-only enforcement
//!
//! `ureq` exposes mutation methods (`post`, `put`, `patch`, `delete`) on the
//! same agent used for reads. To keep the domain provably read-only, **all**
//! HTTP access is confined to [`client`]. Other modules in `src/loki/` must
//! not import `ureq::Agent`, call `ureq::agent(`, or use any non-GET method.
//! A unit test in [`client`] enforces this by grep.
//!
//! Loki exposes write endpoints — log ingestion (`/loki/api/v1/push`) and log
//! deletion (`/loki/api/v1/delete`) — so the chokepoint is genuinely guarding
//! something, not just decorative.

pub mod client;
pub mod common_args;
pub mod query;
pub mod range;
pub mod runner;

use crate::output::Outcome;

use anyhow::Result;
use clap::Subcommand;

/// Subcommands of `sak loki`.
#[derive(Subcommand)]
pub enum LokiCommand {
    /// Run an instant LogQL query
    Query(query::QueryArgs),
    /// Run a range LogQL query
    QueryRange(range::RangeArgs),
}

/// Dispatch a `sak loki` subcommand. Synchronous — no tokio runtime.
pub fn run(cmd: &LokiCommand) -> Result<Outcome> {
    match cmd {
        LokiCommand::Query(args) => query::run(args),
        LokiCommand::QueryRange(args) => range::run(args),
    }
}
