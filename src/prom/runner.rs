//! The shared `run()` skeleton for the single-fetch Prometheus commands.
//!
//! Every `sak prom <cmd>` follows the same shape: resolve the endpoint, GET one
//! `/api/v1/...` path, short-circuit `--json`, otherwise turn the response into
//! output rows and stream them through a [`BoundedWriter`] with a `--limit`
//! bound and a wrote-anything exit code. [`run_prom`] owns all of that
//! scaffolding so each command supplies only its path and a
//! `&Value -> Result<Vec<String>>` row builder.
//!
//! Two prom commands deliberately do **not** use this:
//! - `histogram` fetches twice (cumulative + rate) and dumps a combined object
//!   for `--json`, so it keeps its own `run`.
//! - the `am` (Alertmanager) subcommands resolve `ALERTMANAGER_URL`, talk to the
//!   v2 API, and use envelope-less `get_json` — a different fetch path.

use std::io;
use std::process::ExitCode;

use anyhow::Result;
use serde_json::Value;

use crate::output::{BoundedWriter, emit_json};
use crate::prom::client::{PromClient, resolve_endpoint};
use crate::prom::common_args::CommonPromArgs;

/// Run a single-fetch Prometheus command end to end.
///
/// Resolves `PROMETHEUS_URL` (overridable via `--url`), GETs `path`, and:
/// - maps a not-found response to exit code 1;
/// - on `--json`, dumps the raw response through [`emit_json`] honoring
///   `--limit`;
/// - otherwise calls `build_rows`, then streams the returned lines through a
///   [`BoundedWriter`], returning exit code 0 if any line was written and 1 if
///   the result was empty.
pub(super) fn run_prom<F>(common: &CommonPromArgs, path: &str, build_rows: F) -> Result<ExitCode>
where
    F: FnOnce(&Value) -> Result<Vec<String>>,
{
    let endpoint = resolve_endpoint(common.url.as_deref(), "PROMETHEUS_URL")?;
    let client = PromClient::new(endpoint);
    let data = match client.get_prom(path)? {
        Some(v) => v,
        None => return Ok(ExitCode::from(1)),
    };

    if common.json {
        return emit_json(&data, common.limit);
    }

    let rows = build_rows(&data)?;
    emit_lines(&rows, common.limit)
}

/// Stream `lines` through a [`BoundedWriter`], returning exit code 0 if any
/// line was written and 1 otherwise — the shared tail of every `run_prom`.
fn emit_lines(lines: &[String], limit: Option<usize>) -> Result<ExitCode> {
    let stdout = io::stdout();
    let handle = stdout.lock();
    let mut writer = BoundedWriter::new(handle, limit);

    let mut wrote_any = false;
    for line in lines {
        if !writer.write_line(line)? {
            break;
        }
        wrote_any = true;
    }
    writer.flush()?;
    Ok(if wrote_any {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    })
}
