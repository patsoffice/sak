//! Sole chokepoint for blocking HTTP access to the prom/loki APIs.
//!
//! Every `ureq::Agent` construction and every non-GET method in the shared
//! sync-HTTP layer lives here. The [`tests::no_mutation_methods_outside_client_module`]
//! grep test enforces it on every run that compiles this module (any build
//! with the `prom` or `loki` cargo feature).
//!
//! `ureq` does not provide a read-only client variant, so this convention is
//! the only thing that keeps the prom/loki domains provably free of writes.
//! Both APIs expose write endpoints (Prometheus's `/api/v1/admin/tsdb/*`
//! with `--web.enable-admin-api`, Loki's `/loki/api/v1/push` ingest and
//! `/loki/api/v1/delete`), so the chokepoint is genuinely guarding
//! something, not just decorative.

use std::io::Read;

use anyhow::{Context, Result, bail};
use serde_json::Value;
use ureq::Agent;

/// A connected blocking-HTTP client. Bundles the ureq agent and the resolved
/// base URL so callers don't have to thread both arguments through every
/// helper. The prom/loki domain clients (`PromClient`, `LokiClient`) wrap
/// this with their own response-envelope unwrapping.
pub struct SyncClient {
    agent: Agent,
    base_url: String,
}

impl SyncClient {
    /// Build a client against an explicit base URL (e.g. `https://prom:9090`).
    ///
    /// Trailing slashes are stripped so callers can pass either form of the
    /// URL and the concatenated `<base><path>` still produces a well-formed
    /// absolute URL.
    pub fn new(base_url: impl Into<String>) -> Self {
        let mut base = base_url.into();
        while base.ends_with('/') {
            base.pop();
        }
        Self {
            agent: Agent::new(),
            base_url: base,
        }
    }

    /// Issue a `GET` against `<base_url><path>` and return the response body
    /// parsed as JSON. Returns `Ok(None)` on HTTP 404 so callers can map
    /// "not found" to sak's exit code 1 without losing the ability to
    /// surface other errors as exit code 2 (mirrors `k8s::client::get_dyn`,
    /// `lxc::client::get_json`, `docker::client::get_json`).
    ///
    /// This is the raw form — no response-envelope unwrapping. Prometheus
    /// endpoints return a `{status, data, errorType?, error?}` envelope and
    /// Loki endpoints a `{status, data}` envelope; each domain unwraps its
    /// own shape on top of this call.
    ///
    /// The body is read via `into_reader()` rather than `into_string()`:
    /// the latter caps at 10 MiB and real-world responses (Prometheus
    /// `/api/v1/targets` with its full `discoveredLabels` set, wide Loki
    /// log queries) routinely exceed that. Input is otherwise unbounded —
    /// consistent with the rest of sak (kube and the docker/lxc clients also
    /// buffer whole API responses); `--limit` bounds *output*, not the
    /// response body.
    pub fn get_json(&self, path: &str) -> Result<Option<Value>> {
        let url = format!("{}{}", self.base_url, path);
        let response = match self.agent.get(&url).call() {
            Ok(response) => response,
            Err(ureq::Error::Status(404, _)) => return Ok(None),
            Err(ureq::Error::Status(code, response)) => {
                let body = response
                    .into_string()
                    .unwrap_or_else(|_| "<unreadable body>".to_string());
                bail!("GET {url} returned HTTP {code}: {body}");
            }
            Err(e) => return Err(e).with_context(|| format!("GET {url}")),
        };
        let mut body = String::new();
        response
            .into_reader()
            .read_to_string(&mut body)
            .with_context(|| format!("reading response body for GET {url}"))?;
        let value: Value = serde_json::from_str(&body)
            .with_context(|| format!("parsing JSON response for GET {url}"))?;
        Ok(Some(value))
    }
}

#[cfg(test)]
mod tests {
    /// Tokens that must not appear in any `src/sync_http/*.rs` file other
    /// than `client.rs`. The directory walk and comment-skip mechanics live
    /// in [`crate::test_support::assert_no_forbidden_tokens`].
    const FORBIDDEN_TOKENS: &[&str] = &[
        "ureq::Agent",
        "ureq::agent",
        "ureq::post",
        "ureq::put",
        "ureq::patch",
        "ureq::delete",
        ".post(",
        ".put(",
        ".patch(",
        ".delete(",
    ];

    #[test]
    fn no_mutation_methods_outside_client_module() {
        crate::test_support::assert_no_forbidden_tokens(
            "sync_http",
            FORBIDDEN_TOKENS,
            "ureq agent / mutation methods must be confined to src/sync_http/client.rs",
        );
    }

    #[test]
    fn new_strips_trailing_slashes() {
        let c = super::SyncClient::new("https://prom:9090/");
        assert_eq!(c.base_url, "https://prom:9090");
        let c = super::SyncClient::new("https://prom:9090////");
        assert_eq!(c.base_url, "https://prom:9090");
    }

    #[test]
    fn new_preserves_no_trailing_slash() {
        let c = super::SyncClient::new("https://prom:9090");
        assert_eq!(c.base_url, "https://prom:9090");
    }
}
