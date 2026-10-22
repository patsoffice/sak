//! Shared blocking-HTTP transport for the `prom` and `loki` domains.
//!
//! Prometheus (metrics) and Loki (logs) are sibling JSON-over-HTTP APIs
//! served by the same blocking `ureq` stack, so their plumbing is shared
//! here once: the [`SyncClient`](client::SyncClient) chokepoint that issues
//! every GET, the [`run_single_fetch`] skeleton behind each domain's
//! single-fetch runner, the [`FetchArgs`] trait both domains' `--url` /
//! `--json` / `--limit` flag structs implement, and the small pure helpers
//! (`resolve_endpoint_inner`, [`unix_now`], [`urlencode`]) every command
//! reuses when building request paths.
//!
//! Deliberately *not* shared: response envelopes and result formatting.
//! Prometheus wraps payloads in `{status, data, errorType?, error?}` (errors
//! in-band) while Loki wraps them in `{status, data}` (errors as non-2xx +
//! plain text), so each domain keeps its own unwrap with its own tests. Row
//! rendering (`format_result`, `format_labels`, `extract_strings`) is likewise
//! per-domain presentation. Timestamp units also stay at the call sites:
//! prom sends whole-second epochs, Loki nanosecond epochs (`NANOS_PER_SEC`
//! in each domain's `range.rs` / `series.rs`) — sharing the path builders
//! would hide that unit difference, so only [`unix_now`] (clock, no unit)
//! is shared.

pub mod client;

pub use client::SyncClient;

use std::io;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Result, anyhow};
use serde_json::Value;

use crate::output::{BoundedWriter, Outcome, emit_json};

/// `--json` / `--limit`, as consumed by [`run_single_fetch`]. Implemented by
/// each domain's own flattened `Args` struct so the per-flag help text can
/// keep naming that domain's server and env var. (`--url` is also declared
/// in both structs but resolved in each domain's runner, which knows its env
/// var, so the shared runner never reads it through this trait.)
pub trait FetchArgs {
    /// `--json` passthrough flag.
    fn json(&self) -> bool;
    /// `--limit` output bound.
    fn limit(&self) -> Option<usize>;
}

/// Resolve a server base URL.
///
/// Precedence:
/// 1. The `--url` flag value (passed in as `flag`),
/// 2. The given environment variable,
/// 3. Hard error pointing at the planned auto-discovery follow-up.
///
/// Auto-discovery via a Kubernetes service selector + transparent
/// port-forward is a planned follow-up; for the foundation, URL or env var
/// is required.
pub fn resolve_endpoint_inner(
    flag: Option<&str>,
    env_var: &str,
    env_value: Option<String>,
) -> Result<String> {
    if let Some(url) = flag
        && !url.is_empty()
    {
        return Ok(url.to_string());
    }
    if let Some(url) = env_value
        && !url.is_empty()
    {
        return Ok(url);
    }
    Err(anyhow!(
        "no endpoint URL. Pass --url <URL> or set {env_var}. \
         (Auto-discovery via Kubernetes service + transparent port-forward \
         is a planned follow-up.)"
    ))
}

/// Current unix time in whole seconds. Surfaces a clear error rather than
/// panicking if the system clock is set before the unix epoch. The range and
/// series builders in both domains derive their windows from this; each
/// builder applies its own unit (seconds for prom, nanoseconds for Loki)
/// when rendering request paths.
pub fn unix_now() -> Result<u64> {
    Ok(SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| anyhow!("system clock is before the unix epoch: {e}"))?
        .as_secs())
}

/// Minimal `application/x-www-form-urlencoded` encoder for query parameter
/// values. Encodes anything outside the unreserved set (RFC 3986). Avoids
/// pulling in a dedicated URL crate just to escape PromQL/LogQL braces.
pub fn urlencode(s: &str) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(s.len());
    for byte in s.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char);
            }
            _ => write!(&mut out, "%{:02X}", byte).unwrap(),
        }
    }
    out
}

/// Run a single-fetch command end to end: invoke `fetch` for the response
/// payload, then either dump it raw (`--json`) or turn it into output rows.
///
/// Maps a not-found response to exit code 1; on `--json`, dumps the payload
/// through [`emit_json`] honoring `--limit`; otherwise calls `build_rows`,
/// then streams the returned lines through a [`BoundedWriter`], returning
/// exit code 0 if any line was written and 1 if the result was empty.
pub fn run_single_fetch<A, F>(
    common: &A,
    fetch: impl FnOnce() -> Result<Option<Value>>,
    build_rows: F,
) -> Result<Outcome>
where
    A: FetchArgs,
    F: FnOnce(&Value) -> Result<Vec<String>>,
{
    let data = match fetch()? {
        Some(v) => v,
        None => return Ok(Outcome::NotFound),
    };

    if common.json() {
        return emit_json(&data, common.limit());
    }

    let rows = build_rows(&data)?;
    emit_lines(&rows, common.limit())
}

/// Stream `lines` through a [`BoundedWriter`], returning exit code 0 if any
/// line was written and 1 otherwise — the shared tail of every single-fetch
/// command in both domains.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urlencode_leaves_unreserved_chars() {
        assert_eq!(urlencode("a-b_c.d~e"), "a-b_c.d~e");
    }

    #[test]
    fn urlencode_escapes_query_punctuation() {
        assert_eq!(urlencode("foo bar"), "foo%20bar");
        assert_eq!(urlencode("{app=\"api\"}"), "%7Bapp%3D%22api%22%7D");
    }

    #[test]
    fn urlencode_handles_non_ascii() {
        assert_eq!(urlencode("µs"), "%C2%B5s");
    }
}
