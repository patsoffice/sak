//! SQLite domain — read-only operations against on-disk `.db` files.
//!
//! All commands open the database via [`client::open_readonly`], which uses
//! `SQLITE_OPEN_READ_ONLY` plus `PRAGMA query_only=ON` so writes are blocked
//! at the OS layer *and* by the engine itself. This is a strictly stronger
//! enforcement than the convention-only approach used by the k8s domain.
//!
//! # Read-only enforcement
//!
//! `rusqlite::Connection` exposes mutation methods (`execute`, `execute_batch`,
//! ...) on the same type used for reads. To keep the domain provably read-only,
//! **all** `Connection` access is confined to [`client`]. Other modules in
//! `src/sqlite/` must not import `rusqlite::Connection` or call its mutation
//! methods. A unit test in [`client`] enforces this by grep.
//!
//! # Sync
//!
//! `rusqlite` is synchronous, so unlike the k8s domain there is no tokio
//! runtime — [`run`] dispatches directly.

pub mod client;
pub mod query;
pub mod schema;
pub mod tables;

use std::process::ExitCode;

use anyhow::Result;
use clap::Subcommand;

/// Subcommands of `sak sqlite`.
#[derive(Subcommand)]
pub enum SqliteCommand {
    Query(query::QueryArgs),
    Schema(schema::SchemaArgs),
    Tables(tables::TablesArgs),
}

/// Dispatch a `sak sqlite` subcommand.
pub fn run(cmd: &SqliteCommand) -> Result<ExitCode> {
    match cmd {
        SqliteCommand::Query(args) => query::run(args),
        SqliteCommand::Schema(args) => schema::run(args),
        SqliteCommand::Tables(args) => tables::run(args),
    }
}
