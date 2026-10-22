//! The shared `run()` skeleton for the single-fetch Loki commands.
//!
//! Every `sak loki <cmd>` follows the same shape: resolve the endpoint, GET one
//! `/loki/api/v1/...` path, short-circuit `--json`, otherwise turn the response
//! into output rows. [`run_loki`] owns the endpoint + fetch half of that
//! scaffolding and delegates the row emission to the shared
//! [`crate::sync_http::run_single_fetch`], so each command supplies only its
//! path and a `&Value -> Result<Vec<String>>` row builder. The only
//! differences from the prom twin are the `LOKI_URL` env var and the
//! [`get_loki`](crate::loki::client::LokiClient::get_loki) envelope shape.

use crate::output::Outcome;

use anyhow::Result;
use serde_json::Value;

use crate::loki::client::{LokiClient, resolve_endpoint};
use crate::loki::common_args::CommonLokiArgs;

/// Run a single-fetch Loki command end to end.
///
/// Resolves `LOKI_URL` (overridable via `--url`), GETs `path` with the Loki
/// envelope unwrapped, and emits via the shared single-fetch runner:
/// `--json` dumps the unwrapped `data` payload, otherwise `build_rows` turns
/// it into output lines (exit 0 if any line was written, 1 if the result was
/// empty, 1 on not-found).
pub(super) fn run_loki<F>(common: &CommonLokiArgs, path: &str, build_rows: F) -> Result<Outcome>
where
    F: FnOnce(&Value) -> Result<Vec<String>>,
{
    let endpoint = resolve_endpoint(common.url.as_deref())?;
    let client = LokiClient::new(endpoint);
    crate::sync_http::run_single_fetch(common, || client.get_loki(path), build_rows)
}
