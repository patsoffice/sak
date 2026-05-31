//! The shared `run()` skeleton for the single-fetch Loki commands.
//!
//! Every `sak loki <cmd>` follows the same shape: resolve the endpoint, GET one
//! `/loki/api/v1/...` path, short-circuit `--json`, otherwise turn the response
//! into output rows and stream them through a [`BoundedWriter`] with a
//! `--limit` bound and a wrote-anything exit code. [`run_loki`] owns all of
//! that scaffolding so each command supplies only its path and a
//! `&Value -> Result<Vec<String>>` row builder.
//!
//! This mirrors [`crate::prom::runner::run_prom`]; the only differences are the
//! `LOKI_URL` env var and the [`get_loki`](crate::loki::client::LokiClient::get_loki)
//! envelope shape.

use crate::output::Outcome;
use std::io;

use anyhow::Result;
use serde_json::Value;

use crate::loki::client::{LokiClient, resolve_endpoint};
use crate::loki::common_args::CommonLokiArgs;
use crate::output::{BoundedWriter, emit_json};

/// Run a single-fetch Loki command end to end.
///
/// Resolves `LOKI_URL` (overridable via `--url`), GETs `path`, and:
/// - maps a not-found response to exit code 1;
/// - on `--json`, dumps the unwrapped `data` payload through [`emit_json`]
///   honoring `--limit`;
/// - otherwise calls `build_rows`, then streams the returned lines through a
///   [`BoundedWriter`], returning exit code 0 if any line was written and 1 if
///   the result was empty.
pub(super) fn run_loki<F>(common: &CommonLokiArgs, path: &str, build_rows: F) -> Result<Outcome>
where
    F: FnOnce(&Value) -> Result<Vec<String>>,
{
    let endpoint = resolve_endpoint(common.url.as_deref())?;
    let client = LokiClient::new(endpoint);
    let data = match client.get_loki(path)? {
        Some(v) => v,
        None => return Ok(Outcome::NotFound),
    };

    if common.json {
        return emit_json(&data, common.limit);
    }

    let rows = build_rows(&data)?;
    emit_lines(&rows, common.limit)
}

/// Stream `lines` through a [`BoundedWriter`], returning exit code 0 if any
/// line was written and 1 otherwise — the shared tail of every `run_loki`.
fn emit_lines(lines: &[String], limit: Option<usize>) -> Result<Outcome> {
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
        Outcome::Found
    } else {
        Outcome::NotFound
    })
}
