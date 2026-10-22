//! The shared `run()` skeleton for the single-fetch Prometheus commands.
//!
//! Every `sak prom <cmd>` follows the same shape: resolve the endpoint, GET one
//! `/api/v1/...` path, short-circuit `--json`, otherwise turn the response into
//! output rows. [`run_prom`] owns the endpoint + fetch half of that scaffolding
//! and delegates the row emission to the shared
//! [`crate::sync_http::run_single_fetch`], so each command supplies only its
//! path and a `&Value -> Result<Vec<String>>` row builder.
//!
//! Two prom commands deliberately do **not** use this:
//! - `histogram` fetches twice (cumulative + rate) and dumps a combined object
//!   for `--json`, so it keeps its own `run`.
//! - the `am` (Alertmanager) subcommands resolve `ALERTMANAGER_URL`, talk to the
//!   v2 API, and use envelope-less `get_json` — a different fetch path.

use crate::output::Outcome;

use anyhow::Result;
use serde_json::Value;

use crate::prom::client::{PromClient, resolve_endpoint};
use crate::prom::common_args::CommonPromArgs;

/// Run a single-fetch Prometheus command end to end.
///
/// Resolves `PROMETHEUS_URL` (overridable via `--url`), GETs `path` with the
/// Prometheus envelope unwrapped, and emits via the shared single-fetch
/// runner: `--json` dumps the raw response, otherwise `build_rows` turns the
/// `data` payload into output lines (exit 0 if any line was written, 1 if
/// the result was empty, 1 on not-found).
pub(super) fn run_prom<F>(common: &CommonPromArgs, path: &str, build_rows: F) -> Result<Outcome>
where
    F: FnOnce(&Value) -> Result<Vec<String>>,
{
    let endpoint = resolve_endpoint(common.url.as_deref(), "PROMETHEUS_URL")?;
    let client = PromClient::new(endpoint);
    crate::sync_http::run_single_fetch(common, || client.get_prom(path), build_rows)
}
