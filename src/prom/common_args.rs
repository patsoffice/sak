//! Connection + output flags shared by every Prometheus `sak prom <cmd>`.
//!
//! `--url`, `--json`, and `--limit` were re-declared, identically, in all 13
//! Prometheus command `Args` structs. They live here once and are pulled into
//! each command with `#[command(flatten)]`.
//!
//! The Alertmanager subcommands (`sak prom am …`) deliberately do **not** use
//! this struct: they resolve `ALERTMANAGER_URL` rather than `PROMETHEUS_URL`,
//! talk to the v2 API, and word their `--url` help for Alertmanager — so their
//! flags stay local to `am.rs`. The env-var name is in any case a per-command
//! concern (passed to `resolve_endpoint` in each `run`), not encoded here.

use clap::Args;

/// `--url` / `--json` / `--limit`, shared across the Prometheus commands.
#[derive(Args)]
pub struct CommonPromArgs {
    /// Prometheus base URL (overrides PROMETHEUS_URL env)
    #[arg(long, value_name = "URL")]
    pub url: Option<String>,

    /// Emit the raw JSON response from the Prometheus API
    #[arg(long)]
    pub json: bool,

    /// Maximum number of output lines
    #[arg(long)]
    pub limit: Option<usize>,
}
