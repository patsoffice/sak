//! `sak loki query-range <logql> --since <dur> [--step <dur>]` — range query
//! against `/loki/api/v1/query_range`.
//!
//! A range log query returns `resultType=streams`; a range metric query (e.g.
//! `rate({app="api"}[5m])`) returns `matrix`. Both are handled by
//! [`crate::loki::query::format_result`], so output is the same
//! `<ts><TAB>labels<TAB>line` (streams) / `<labels><TAB><ts><TAB><value>`
//! (matrix) shape as the instant command, with rows sorted for diff-stable
//! output.
//!
//! `--since` sets how far back the window starts from now; `--step` sets the
//! resolution for metric queries (Loki ignores it for log selectors, but it's
//! harmless to send). `end` is always "now", so re-running the same command
//! walks the window forward in real time.
//!
//! Unlike Prometheus, Loki's `start`/`end` are nanosecond Unix epochs, so this
//! module renders timestamps in ns; `step` stays in whole seconds (Loki
//! accepts a bare-seconds step).

use crate::output::Outcome;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Result, anyhow};
use clap::Args;

use crate::duration::parse_duration;
use crate::loki::common_args::CommonLokiArgs;
use crate::loki::query::{format_result, urlencode};
use crate::loki::runner::run_loki;

/// Nanoseconds per second — the multiplier from sak's second-granularity
/// duration windows to the nanosecond Unix epochs Loki's range API expects.
const NANOS_PER_SEC: u64 = 1_000_000_000;

#[derive(Args)]
#[command(
    about = "Run a range LogQL query",
    long_about = "Execute a LogQL range query against `/loki/api/v1/query_range` \
        over the window `[now - since, now]`. A log selector returns streams — \
        one `<ts_ns><TAB><labels><TAB><line>` line per entry; a metric LogQL \
        expression returns a matrix — one `<labels><TAB><ts><TAB><value>` line \
        per sample at `--step` resolution. Rows are sorted.\n\n\
        Durations are compact compound strings: s/m/h/d/w units, chainable \
        (`90s`, `5m`, `2h30m`, `1d`).\n\n\
        Connection: pass --url <http://loki:3100> or set LOKI_URL.",
    after_help = "\
Examples:
  sak loki query-range '{app=\"api\"}' --since 1h          Last hour of one app
  sak loki query-range '{app=\"api\"} |= \"error\"' --since 6h
  sak loki query-range 'rate({app=\"api\"}[5m])' --since 6h --step 5m
  sak loki query-range '{app=\"api\"}' --since 30m --json   Raw JSON for piping"
)]
pub struct RangeArgs {
    #[command(flatten)]
    pub common: CommonLokiArgs,

    /// The LogQL expression to evaluate
    #[arg(value_name = "LOGQL")]
    pub query: String,

    /// How far back the window starts from now (e.g. 1h, 30m, 2d)
    #[arg(long, value_name = "DURATION")]
    pub since: String,

    /// Resolution step between samples for metric queries (e.g. 15s, 1m, 1h)
    #[arg(long, value_name = "DURATION", default_value = "60s")]
    pub step: String,
}

pub fn run(args: &RangeArgs) -> Result<Outcome> {
    let since = parse_duration(&args.since).map_err(|e| anyhow!("--since: {e}"))?;
    let step = parse_duration(&args.step).map_err(|e| anyhow!("--step: {e}"))?;
    if step == 0 {
        return Err(anyhow!("--step must be a non-zero duration"));
    }

    let now = unix_now()?;
    let start = now.saturating_sub(since);
    let path = build_range_path(&args.query, start, now, step);

    run_loki(&args.common, &path, |data| {
        let mut lines = format_result(data)?;
        lines.sort();
        Ok(lines)
    })
}

/// Build the `/loki/api/v1/query_range` request path. `start`/`end` are taken
/// in whole seconds and rendered as nanosecond Unix epochs (Loki's expected
/// unit); `step` is sent in whole seconds. Pure so the parameter encoding is
/// unit-testable without a clock or a server.
fn build_range_path(query: &str, start_secs: u64, end_secs: u64, step_secs: u64) -> String {
    format!(
        "/loki/api/v1/query_range?query={}&start={}&end={}&step={}",
        urlencode(query),
        start_secs.saturating_mul(NANOS_PER_SEC),
        end_secs.saturating_mul(NANOS_PER_SEC),
        step_secs
    )
}

/// Current unix time in whole seconds. Surfaces a clear error rather than
/// panicking if the system clock is set before the unix epoch (mirrors
/// `crate::loki::series::unix_now` / `prom::range::unix_now`).
fn unix_now() -> Result<u64> {
    Ok(SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| anyhow!("system clock is before the unix epoch: {e}"))?
        .as_secs())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_path_encodes_query_and_renders_ns() {
        let path = build_range_path("{app=\"api\"}", 1_000, 4_600, 60);
        assert_eq!(
            path,
            "/loki/api/v1/query_range?query=%7Bapp%3D%22api%22%7D\
             &start=1000000000000&end=4600000000000&step=60"
        );
    }

    #[test]
    fn build_path_simple_query() {
        let path = build_range_path("{app=\"api\"}", 100, 200, 15);
        assert_eq!(
            path,
            "/loki/api/v1/query_range?query=%7Bapp%3D%22api%22%7D\
             &start=100000000000&end=200000000000&step=15"
        );
    }
}
