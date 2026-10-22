//! Client for HTTP access to a Grafana Loki API.
//!
//! All HTTP goes through the shared [`crate::sync_http::client::SyncClient`]
//! chokepoint — every `ureq::Agent` construction and every non-GET method
//! lives there, enforced by its own grep test. This module adds only the Loki
//! response-envelope shape on top ([`LokiClient::get_loki`]). The
//! [`tests::no_mutation_methods_outside_client_module`] grep test still
//! scans `src/loki/` so no command file can grow its own HTTP access.

use anyhow::{Result, anyhow, bail};
use serde_json::Value;

use crate::sync_http::SyncClient;

/// A connected Loki client. Wraps the shared blocking GET client with the
/// Loki `{status, data}` envelope unwrapping.
pub struct LokiClient {
    inner: SyncClient,
}

impl LokiClient {
    /// Build a client against an explicit base URL (e.g. `https://loki:3100`).
    ///
    /// Trailing slashes are stripped so callers can pass either form of the
    /// URL and the concatenated `<base><path>` still produces a well-formed
    /// absolute URL.
    pub fn new(base_url: impl Into<String>) -> Self {
        Self {
            inner: SyncClient::new(base_url),
        }
    }

    /// Issue a `GET` against `<base_url><path>` and return the response body
    /// parsed as JSON. Returns `Ok(None)` on HTTP 404 so callers can map
    /// "not found" to sak's exit code 1 without losing the ability to
    /// surface other errors as exit code 2 (mirrors `k8s::client::get_dyn`,
    /// `prom::client::get_json`).
    ///
    /// This is the raw form — no Loki response-envelope unwrapping. For the
    /// JSON-envelope endpoints (`/loki/api/v1/*`), use
    /// [`LokiClient::get_loki`], which unwraps the `{status, data}` wrapper.
    pub fn get_json(&self, path: &str) -> Result<Option<Value>> {
        self.inner.get_json(path)
    }

    /// Issue a `GET` against a Loki `/loki/api/v1/*` endpoint, unwrap the
    /// response envelope `{status, data}`, and return the `data` field.
    ///
    /// Returns `Ok(None)` on HTTP 404. Bails when `status` is present and not
    /// `success`. If the envelope is missing entirely (a protocol violation)
    /// surfaces a clear error rather than silently returning the raw body.
    pub fn get_loki(&self, path: &str) -> Result<Option<Value>> {
        let raw = match self.get_json(path)? {
            Some(v) => v,
            None => return Ok(None),
        };
        let data = unwrap_loki_envelope(&raw, path)?;
        Ok(Some(data))
    }
}

/// Pure helper that extracts `data` from a Loki response envelope and converts
/// a non-`success` status into an `anyhow::Error`. Split out from
/// [`LokiClient::get_loki`] so it's unit-testable on hand-built fixtures
/// without standing up an HTTP server.
///
/// Loki-specific (success-with-`data`, errors arriving as non-2xx + plain
/// text rather than in-band fields), so it stays in this domain rather than
/// moving to the shared `sync_http` module. Unlike Prometheus, the in-band
/// envelope carries no `errorType`/`error` fields — on an unexpected `status`
/// value we surface the status itself rather than hunting for fields that
/// won't be there.
fn unwrap_loki_envelope(raw: &Value, path: &str) -> Result<Value> {
    let status = raw
        .get("status")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("Loki response for {path} has no `status` field"))?;
    match status {
        "success" => {
            let data = raw
                .get("data")
                .ok_or_else(|| anyhow!("Loki success response for {path} has no `data` field"))?;
            Ok(data.clone())
        }
        other => bail!("Loki response for {path} has non-success status {other:?}"),
    }
}

/// Resolve a Loki base URL.
///
/// Precedence:
/// 1. The `--url` flag value (passed in as `flag`),
/// 2. The `LOKI_URL` environment variable,
/// 3. Hard error pointing at the planned auto-discovery follow-up.
///
/// Auto-discovery via a Kubernetes service selector + transparent port-forward
/// is a planned follow-up (the same deferral as `prom`); for now, URL or env
/// var is required.
pub fn resolve_endpoint(flag: Option<&str>) -> Result<String> {
    crate::sync_http::resolve_endpoint_inner(flag, "LOKI_URL", std::env::var("LOKI_URL").ok())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use crate::sync_http::resolve_endpoint_inner;

    /// Tokens that must not appear in any `src/loki/*.rs` file other than
    /// `client.rs`. The directory walk and comment-skip mechanics live in
    /// [`crate::test_support::assert_no_forbidden_tokens`]. The actual
    /// `ureq` construction lives in the shared `sync_http` chokepoint; this
    /// test keeps the loki domain from growing its own HTTP access.
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
            "loki",
            FORBIDDEN_TOKENS,
            "ureq agent / mutation methods must be confined to the sync_http chokepoint; \
             no src/loki/*.rs outside client.rs may touch HTTP",
        );
    }

    #[test]
    fn unwrap_envelope_returns_data_on_success() {
        let raw = json!({"status": "success", "data": {"resultType": "streams", "result": []}});
        let data = super::unwrap_loki_envelope(&raw, "/loki/api/v1/query").unwrap();
        assert_eq!(data, json!({"resultType": "streams", "result": []}));
    }

    #[test]
    fn unwrap_envelope_returns_array_data() {
        let raw = json!({"status": "success", "data": ["app", "namespace"]});
        let data = super::unwrap_loki_envelope(&raw, "/loki/api/v1/labels").unwrap();
        assert_eq!(data, json!(["app", "namespace"]));
    }

    #[test]
    fn unwrap_envelope_bails_on_non_success_status() {
        let raw = json!({"status": "error"});
        let err = super::unwrap_loki_envelope(&raw, "/loki/api/v1/query").unwrap_err();
        assert!(format!("{err}").contains("non-success status"));
    }

    #[test]
    fn unwrap_envelope_bails_when_status_missing() {
        let raw = json!({"data": []});
        let err = super::unwrap_loki_envelope(&raw, "/loki/api/v1/query").unwrap_err();
        assert!(format!("{err}").contains("`status` field"));
    }

    #[test]
    fn unwrap_envelope_bails_on_success_with_no_data() {
        let raw = json!({"status": "success"});
        let err = super::unwrap_loki_envelope(&raw, "/loki/api/v1/query").unwrap_err();
        assert!(format!("{err}").contains("`data` field"));
    }

    #[test]
    fn resolve_endpoint_prefers_flag() {
        let resolved = resolve_endpoint_inner(
            Some("http://flag:3100"),
            "LOKI_URL",
            Some("http://env:3100".to_string()),
        )
        .unwrap();
        assert_eq!(resolved, "http://flag:3100");
    }

    #[test]
    fn resolve_endpoint_falls_back_to_env() {
        let resolved =
            resolve_endpoint_inner(None, "LOKI_URL", Some("http://env:3100".to_string())).unwrap();
        assert_eq!(resolved, "http://env:3100");
    }

    #[test]
    fn resolve_endpoint_errors_when_neither_set() {
        let err = resolve_endpoint_inner(None, "LOKI_URL", None).unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("LOKI_URL"), "got: {msg}");
        assert!(msg.contains("--url"), "got: {msg}");
    }

    #[test]
    fn resolve_endpoint_treats_empty_flag_as_unset() {
        let resolved =
            resolve_endpoint_inner(Some(""), "LOKI_URL", Some("http://env:3100".to_string()))
                .unwrap();
        assert_eq!(resolved, "http://env:3100");
    }

    #[test]
    fn resolve_endpoint_treats_empty_env_as_unset() {
        let err = resolve_endpoint_inner(None, "LOKI_URL", Some(String::new())).unwrap_err();
        assert!(format!("{err}").contains("LOKI_URL"));
    }
}
