//! Connection + output flags shared by every Loki `sak loki <cmd>`.
//!
//! `--url`, `--json`, and `--limit` would otherwise be re-declared,
//! identically, in every Loki command `Args` struct. They live here once and
//! are pulled into each command with `#[command(flatten)]` — the same pattern
//! as [`crate::prom::common_args::CommonPromArgs`].

use clap::Args;

/// `--url` / `--json` / `--limit`, shared across the Loki commands.
#[derive(Args)]
pub struct CommonLokiArgs {
    /// Loki base URL (overrides LOKI_URL env)
    #[arg(long, value_name = "URL")]
    pub url: Option<String>,

    /// Emit the raw JSON `data` payload from the Loki API
    #[arg(long)]
    pub json: bool,

    /// Maximum number of output lines
    #[arg(long)]
    pub limit: Option<usize>,
}
