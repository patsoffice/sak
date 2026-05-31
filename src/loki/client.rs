//! Sole chokepoint for HTTP access to a Grafana Loki API.
//!
//! Every other module in `src/loki/` must route HTTP through the helpers
//! exposed here. Importing `ureq::Agent`, calling `ureq::agent`, or using any
//! non-GET method anywhere else in the domain is forbidden, and the
//! [`tests::no_mutation_methods_outside_client_module`] grep test enforces it
//! on every `cargo test --features loki` run.
//!
//! `ureq` does not provide a read-only client variant, so this convention is
//! the only thing that keeps the domain provably free of writes. Loki exposes
//! admin write endpoints — `/loki/api/v1/delete` (log deletion) and the
//! ingester push API `/loki/api/v1/push` — so the chokepoint is genuinely
//! guarding something, not just decorative.
//!
//! This mirrors [`crate::prom::client`] (the metric-side counterpart) almost
//! verbatim; the one shape difference is [`LokiClient::get_loki`], whose
//! success envelope is `{status, data}` with no `errorType`/`error` fields —
//! Loki signals query errors with a non-2xx HTTP status and a plain-text body,
//! not an in-band `{status:"error"}` object the way Prometheus does.

use std::io::Read;

use anyhow::{Context, Result, anyhow, bail};
use serde_json::Value;
use ureq::Agent;

/// A connected Loki client. Bundles the ureq agent and the resolved base URL
/// so callers don't have to thread both arguments through every helper.
pub struct LokiClient {
    agent: Agent,
    base_url: String,
}

impl LokiClient {
    /// Build a client against an explicit base URL (e.g. `https://loki:3100`).
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
    /// `prom::client::get_json`).
    ///
    /// This is the raw form — no Loki response-envelope unwrapping. For the
    /// JSON-envelope endpoints (`/loki/api/v1/*`), use
    /// [`LokiClient::get_loki`], which unwraps the `{status, data}` wrapper.
    ///
    /// The body is read via `into_reader()` rather than `into_string()`:
    /// the latter caps at 10 MiB and a wide log query easily exceeds that.
    /// Input is otherwise unbounded — consistent with the rest of sak (the
    /// prom/kube/docker/lxc clients also buffer whole API responses);
    /// `--limit` bounds *output*, not the response body.
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
/// Loki's success envelope is `{status:"success", data:...}`. Unlike
/// Prometheus, the in-band envelope carries no `errorType`/`error` fields —
/// errors arrive as a non-2xx HTTP status with a plain-text body (handled in
/// [`LokiClient::get_json`]) — so on an unexpected `status` value we surface
/// the status itself rather than hunting for fields that won't be there.
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
        other => Err(anyhow!(
            "Loki response for {path} has non-success status {other:?}"
        )),
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
    resolve_endpoint_inner(flag, std::env::var("LOKI_URL").ok())
}

/// Inner pure form of [`resolve_endpoint`] for unit testing — accepts the
/// env-var value as a parameter so tests don't have to mutate process-wide
/// env state (which is `unsafe` in Rust 2024 and racy across tests).
fn resolve_endpoint_inner(flag: Option<&str>, env_value: Option<String>) -> Result<String> {
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
        "no endpoint URL. Pass --url <URL> or set LOKI_URL. \
         (Auto-discovery via Kubernetes service + transparent port-forward \
         is a planned follow-up.)"
    ))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    /// Tokens that must not appear in any `src/loki/*.rs` file other than
    /// `client.rs`. The directory walk and comment-skip mechanics live in
    /// [`crate::test_support::assert_no_forbidden_tokens`].
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
            "ureq agent / mutation methods must be confined to src/loki/client.rs",
        );
    }

    #[test]
    fn new_strips_trailing_slashes() {
        let c = super::LokiClient::new("https://loki:3100/");
        assert_eq!(c.base_url, "https://loki:3100");
        let c = super::LokiClient::new("https://loki:3100////");
        assert_eq!(c.base_url, "https://loki:3100");
    }

    #[test]
    fn new_preserves_no_trailing_slash() {
        let c = super::LokiClient::new("https://loki:3100");
        assert_eq!(c.base_url, "https://loki:3100");
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
        let resolved = super::resolve_endpoint_inner(
            Some("http://flag:3100"),
            Some("http://env:3100".to_string()),
        )
        .unwrap();
        assert_eq!(resolved, "http://flag:3100");
    }

    #[test]
    fn resolve_endpoint_falls_back_to_env() {
        let resolved =
            super::resolve_endpoint_inner(None, Some("http://env:3100".to_string())).unwrap();
        assert_eq!(resolved, "http://env:3100");
    }

    #[test]
    fn resolve_endpoint_errors_when_neither_set() {
        let err = super::resolve_endpoint_inner(None, None).unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("LOKI_URL"), "got: {msg}");
        assert!(msg.contains("--url"), "got: {msg}");
    }

    #[test]
    fn resolve_endpoint_treats_empty_flag_as_unset() {
        let resolved =
            super::resolve_endpoint_inner(Some(""), Some("http://env:3100".to_string())).unwrap();
        assert_eq!(resolved, "http://env:3100");
    }

    #[test]
    fn resolve_endpoint_treats_empty_env_as_unset() {
        let err = super::resolve_endpoint_inner(None, Some(String::new())).unwrap_err();
        assert!(format!("{err}").contains("LOKI_URL"));
    }
}
