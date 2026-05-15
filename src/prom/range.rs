//! `sak prom query-range <promql> --since <dur> [--step <dur>]` — range
//! query against `/api/v1/query_range`.
//!
//! A range query always returns `resultType=matrix`, so output formatting
//! is delegated to [`crate::prom::query::format_result`] — each sample is
//! one `<labels><TAB><ts><TAB><value>` line, with rows sorted for
//! diff-stable output.
//!
//! `--since` sets how far back the window starts from now; `--step` sets
//! the resolution (default `60s`). `end` is always "now", so re-running the
//! same command walks the window forward in real time.

use std::io;
use std::process::ExitCode;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Result, anyhow};
use clap::Args;
use serde_json::Value;

use crate::output::BoundedWriter;
use crate::prom::client::{PromClient, resolve_endpoint};
use crate::prom::duration::parse_duration;
use crate::prom::query::{format_result, urlencode};

#[derive(Args)]
#[command(
    about = "Run a range PromQL query",
    long_about = "Execute a PromQL range query against `/api/v1/query_range` \
        over the window `[now - since, now]` at `--step` resolution. A range \
        query always returns a matrix, so output is one \
        `<labels><TAB><ts><TAB><value>` line per sample, sorted.\n\n\
        Durations are compact compound strings: s/m/h/d/w units, chainable \
        (`90s`, `5m`, `2h30m`, `1d`).\n\n\
        Connection: pass --url <http://prom:9090> or set PROMETHEUS_URL.",
    after_help = "\
Examples:
  sak prom query-range 'up' --since 1h               up{} over the last hour
  sak prom query-range 'rate(http_requests_total[5m])' --since 6h --step 5m
  sak prom query-range 'up' --since 30m --json       Raw JSON for piping"
)]
pub struct RangeArgs {
    /// The PromQL expression to evaluate
    #[arg(value_name = "PROMQL")]
    pub query: String,

    /// How far back the window starts from now (e.g. 1h, 30m, 2d)
    #[arg(long, value_name = "DURATION")]
    pub since: String,

    /// Resolution step between samples (e.g. 15s, 1m, 1h)
    #[arg(long, value_name = "DURATION", default_value = "60s")]
    pub step: String,

    /// Prometheus base URL (overrides PROMETHEUS_URL env)
    #[arg(long, value_name = "URL")]
    pub url: Option<String>,

    /// Emit the raw JSON response from /api/v1/query_range
    #[arg(long)]
    pub json: bool,

    /// Maximum number of output lines
    #[arg(long)]
    pub limit: Option<usize>,
}

pub fn run(args: &RangeArgs) -> Result<ExitCode> {
    let since = parse_duration(&args.since).map_err(|e| anyhow!("--since: {e}"))?;
    let step = parse_duration(&args.step).map_err(|e| anyhow!("--step: {e}"))?;
    if step == 0 {
        return Err(anyhow!("--step must be a non-zero duration"));
    }

    let now = unix_now()?;
    let start = now.saturating_sub(since);
    let path = build_range_path(&args.query, start, now, step);

    let endpoint = resolve_endpoint(args.url.as_deref(), "PROMETHEUS_URL")?;
    let client = PromClient::new(endpoint);
    let data = match client.get_prom(&path)? {
        Some(v) => v,
        None => return Ok(ExitCode::from(1)),
    };

    if args.json {
        return emit_json(&data, args.limit);
    }

    let mut lines = format_result(&data)?;
    lines.sort();

    let stdout = io::stdout();
    let handle = stdout.lock();
    let mut writer = BoundedWriter::new(handle, args.limit);

    let mut wrote_any = false;
    for line in &lines {
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

/// Build the `/api/v1/query_range` request path. Pure so the parameter
/// encoding is unit-testable without a clock or a server.
fn build_range_path(query: &str, start: u64, end: u64, step: u64) -> String {
    format!(
        "/api/v1/query_range?query={}&start={}&end={}&step={}",
        urlencode(query),
        start,
        end,
        step
    )
}

/// Current unix time in whole seconds. Surfaces a clear error rather than
/// panicking if the system clock is set before the unix epoch.
fn unix_now() -> Result<u64> {
    Ok(SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| anyhow!("system clock is before the unix epoch: {e}"))?
        .as_secs())
}

fn emit_json(data: &Value, limit: Option<usize>) -> Result<ExitCode> {
    let stdout = io::stdout();
    let handle = stdout.lock();
    let mut writer = BoundedWriter::new(handle, limit);
    let pretty = serde_json::to_string_pretty(data)?;
    for line in pretty.lines() {
        if !writer.write_line(line)? {
            break;
        }
    }
    writer.flush()?;
    Ok(ExitCode::SUCCESS)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_path_encodes_query_and_appends_params() {
        let path = build_range_path("up{job=\"node\"}", 1_000, 4_600, 60);
        assert_eq!(
            path,
            "/api/v1/query_range?query=up%7Bjob%3D%22node%22%7D\
             &start=1000&end=4600&step=60"
        );
    }

    #[test]
    fn build_path_simple_query() {
        let path = build_range_path("up", 100, 200, 15);
        assert_eq!(
            path,
            "/api/v1/query_range?query=up&start=100&end=200&step=15"
        );
    }
}
