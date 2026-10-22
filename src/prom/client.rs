//! Client for HTTP access to a Prometheus or Alertmanager API.
//!
//! All HTTP goes through the shared [`crate::sync_http::client::SyncClient`]
//! chokepoint — every `ureq::Agent` construction and every non-GET method
//! lives there, enforced by its own grep test. This module adds only the
//! Prometheus response-envelope shape on top ([`PromClient::get_prom`]).
//! The [`tests::no_mutation_methods_outside_client_module`] grep test still
//! scans `src/prom/` so no command file can grow its own HTTP access.

use anyhow::{Result, anyhow, bail};
use serde_json::Value;

use crate::sync_http::SyncClient;

/// A connected Prometheus / Alertmanager client. Wraps the shared blocking
/// GET client with the Prometheus `{status, data, errorType?, error?}`
/// envelope unwrapping.
pub struct PromClient {
    inner: SyncClient,
}

impl PromClient {
    /// Build a client against an explicit base URL (e.g. `https://prom:9090`).
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
    /// `lxc::client::get_json`, `docker::client::get_json`).
    ///
    /// This is the raw form — no Prometheus response-envelope unwrapping.
    /// Used by Alertmanager endpoints (`/api/v2/*`) which return JSON arrays
    /// directly. For Prometheus `/api/v1/*` endpoints, use
    /// [`PromClient::get_prom`], which unwraps the
    /// `{status, data, errorType?, error?}` envelope.
    pub fn get_json(&self, path: &str) -> Result<Option<Value>> {
        self.inner.get_json(path)
    }

    /// Issue a `GET` against a Prometheus `/api/v1/*` endpoint, unwrap the
    /// response envelope `{status, data, errorType?, error?}`, and return
    /// the `data` field.
    ///
    /// Returns `Ok(None)` on HTTP 404. Bails with `errorType` + `error` when
    /// `status=error`. If the envelope is missing entirely (a protocol
    /// violation) surfaces a clear error rather than silently returning the
    /// raw body.
    pub fn get_prom(&self, path: &str) -> Result<Option<Value>> {
        let raw = match self.get_json(path)? {
            Some(v) => v,
            None => return Ok(None),
        };
        let data = unwrap_prom_envelope(&raw, path)?;
        Ok(Some(data))
    }
}

/// Pure helper that extracts `data` from a Prometheus response envelope and
/// converts `status=error` into an `anyhow::Error`. Split out from
/// [`PromClient::get_prom`] so it's unit-testable on hand-built fixtures
/// without standing up an HTTP server.
///
/// Prometheus-specific (the `errorType`/`error` in-band error shape has no
/// Loki counterpart), so it stays in this domain rather than moving to the
/// shared `sync_http` module.
fn unwrap_prom_envelope(raw: &Value, path: &str) -> Result<Value> {
    let status = raw
        .get("status")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("Prometheus response for {path} has no `status` field"))?;
    match status {
        "success" => {
            let data = raw.get("data").ok_or_else(|| {
                anyhow!("Prometheus success response for {path} has no `data` field")
            })?;
            Ok(data.clone())
        }
        "error" => {
            let error_type = raw
                .get("errorType")
                .and_then(Value::as_str)
                .unwrap_or("<no errorType>");
            let error = raw
                .get("error")
                .and_then(Value::as_str)
                .unwrap_or("<no error>");
            Err(anyhow!(
                "Prometheus returned error for {path}: {error_type}: {error}"
            ))
        }
        other => bail!("Prometheus response for {path} has unknown status {other:?}"),
    }
}

/// Resolve a Prometheus or Alertmanager base URL.
///
/// Precedence:
/// 1. The `--url` flag value (passed in as `flag`),
/// 2. The given environment variable (e.g. `PROMETHEUS_URL`,
///    `ALERTMANAGER_URL`),
/// 3. Hard error pointing at the planned auto-discovery follow-up.
///
/// Auto-discovery via a Kubernetes service selector + transparent
/// port-forward is a planned follow-up; for the foundation, URL or env var
/// is required.
pub fn resolve_endpoint(flag: Option<&str>, env_var: &str) -> Result<String> {
    crate::sync_http::resolve_endpoint_inner(flag, env_var, std::env::var(env_var).ok())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use crate::sync_http::resolve_endpoint_inner;

    /// Tokens that must not appear in any `src/prom/*.rs` file other than
    /// `client.rs`. The directory walk and comment-skip mechanics live in
    /// [`crate::test_support::assert_no_forbidden_tokens`]. The actual
    /// `ureq` construction lives in the shared `sync_http` chokepoint; this
    /// test keeps the prom domain from growing its own HTTP access.
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
            "prom",
            FORBIDDEN_TOKENS,
            "ureq agent / mutation methods must be confined to the sync_http chokepoint; \
             no src/prom/*.rs outside client.rs may touch HTTP",
        );
    }

    #[test]
    fn unwrap_envelope_returns_data_on_success() {
        let raw = json!({"status": "success", "data": {"result": [1, 2, 3]}});
        let data = super::unwrap_prom_envelope(&raw, "/api/v1/query").unwrap();
        assert_eq!(data, json!({"result": [1, 2, 3]}));
    }

    #[test]
    fn unwrap_envelope_bails_on_error_status() {
        let raw = json!({
            "status": "error",
            "errorType": "bad_data",
            "error": "invalid parameter \"query\""
        });
        let err = super::unwrap_prom_envelope(&raw, "/api/v1/query").unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("bad_data"), "got: {msg}");
        assert!(msg.contains("invalid parameter"), "got: {msg}");
    }

    #[test]
    fn unwrap_envelope_bails_when_status_missing() {
        let raw = json!({"data": {"result": []}});
        let err = super::unwrap_prom_envelope(&raw, "/api/v1/query").unwrap_err();
        assert!(format!("{err}").contains("`status` field"));
    }

    #[test]
    fn unwrap_envelope_bails_on_success_with_no_data() {
        let raw = json!({"status": "success"});
        let err = super::unwrap_prom_envelope(&raw, "/api/v1/query").unwrap_err();
        assert!(format!("{err}").contains("`data` field"));
    }

    #[test]
    fn unwrap_envelope_bails_on_unknown_status() {
        let raw = json!({"status": "weird"});
        let err = super::unwrap_prom_envelope(&raw, "/api/v1/query").unwrap_err();
        assert!(format!("{err}").contains("unknown status"));
    }

    #[test]
    fn resolve_endpoint_prefers_flag() {
        let resolved = resolve_endpoint_inner(
            Some("http://flag:9090"),
            "PROMETHEUS_URL",
            Some("http://env:9090".to_string()),
        )
        .unwrap();
        assert_eq!(resolved, "http://flag:9090");
    }

    #[test]
    fn resolve_endpoint_falls_back_to_env() {
        let resolved =
            resolve_endpoint_inner(None, "PROMETHEUS_URL", Some("http://env:9090".to_string()))
                .unwrap();
        assert_eq!(resolved, "http://env:9090");
    }

    #[test]
    fn resolve_endpoint_errors_when_neither_set() {
        let err = resolve_endpoint_inner(None, "PROMETHEUS_URL", None).unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("PROMETHEUS_URL"), "got: {msg}");
        assert!(msg.contains("--url"), "got: {msg}");
    }

    #[test]
    fn resolve_endpoint_treats_empty_flag_as_unset() {
        let resolved = resolve_endpoint_inner(
            Some(""),
            "PROMETHEUS_URL",
            Some("http://env:9090".to_string()),
        )
        .unwrap();
        assert_eq!(resolved, "http://env:9090");
    }

    #[test]
    fn resolve_endpoint_treats_empty_env_as_unset() {
        let err = resolve_endpoint_inner(None, "PROMETHEUS_URL", Some(String::new())).unwrap_err();
        assert!(format!("{err}").contains("PROMETHEUS_URL"));
    }
}
