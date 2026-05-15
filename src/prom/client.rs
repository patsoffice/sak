//! Sole chokepoint for HTTP access to a Prometheus or Alertmanager API.
//!
//! Every other module in `src/prom/` must route HTTP through the helpers
//! exposed here. Importing `ureq::Agent`, calling `ureq::agent`, or using any
//! non-GET method anywhere else in the domain is forbidden, and the
//! [`tests::no_mutation_methods_outside_client_module`] grep test enforces it
//! on every `cargo test --features prom` run.
//!
//! `ureq` does not provide a read-only client variant, so this convention is
//! the only thing that keeps the domain provably free of writes. Prometheus
//! exposes admin write endpoints under `/api/v1/admin/tsdb/*` when the server
//! is started with `--web.enable-admin-api`, so the chokepoint is genuinely
//! guarding something, not just decorative.

use std::io::Read;

use anyhow::{Context, Result, anyhow, bail};
use serde_json::Value;
use ureq::Agent;

/// A connected Prometheus / Alertmanager client. Bundles the ureq agent and
/// the resolved base URL so callers don't have to thread both arguments
/// through every helper.
pub struct PromClient {
    agent: Agent,
    base_url: String,
}

impl PromClient {
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
    /// This is the raw form — no Prometheus response-envelope unwrapping.
    /// Used by Alertmanager endpoints (`/api/v2/*`) which return JSON arrays
    /// directly. For Prometheus `/api/v1/*` endpoints, use
    /// [`PromClient::get_prom`], which unwraps the
    /// `{status, data, errorType?, error?}` envelope.
    ///
    /// The body is read via `into_reader()` rather than `into_string()`:
    /// the latter caps at 10 MiB and `/api/v1/targets` (which carries the
    /// full `discoveredLabels` set for every target) routinely exceeds that
    /// on a real cluster. Input is otherwise unbounded — consistent with
    /// the rest of sak (kube and the docker/lxc clients also buffer whole
    /// API responses); `--limit` bounds *output*, not the response body.
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
        other => Err(anyhow!(
            "Prometheus response for {path} has unknown status {other:?}"
        )),
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
    resolve_endpoint_inner(flag, env_var, std::env::var(env_var).ok())
}

/// Inner pure form of [`resolve_endpoint`] for unit testing — accepts the
/// env-var value as a parameter so tests don't have to mutate process-wide
/// env state (which is `unsafe` in Rust 2024 and racy across tests).
fn resolve_endpoint_inner(
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

#[cfg(test)]
mod tests {
    use std::ffi::OsStr;
    use std::fs;
    use std::path::PathBuf;

    use serde_json::json;

    /// Tokens that must not appear in any `src/prom/*.rs` file other than
    /// `client.rs`. Comments are exempt — the skip logic below ignores any
    /// line whose first non-whitespace characters are `//`.
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
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/prom");
        let entries = fs::read_dir(&dir).expect("read src/prom");

        let mut violations = Vec::new();
        for entry in entries {
            let entry = entry.expect("dir entry");
            let path = entry.path();
            if path.extension() != Some(OsStr::new("rs")) {
                continue;
            }
            if path.file_name() == Some(OsStr::new("client.rs")) {
                continue;
            }

            let content = fs::read_to_string(&path).expect("read source file");
            for (idx, line) in content.lines().enumerate() {
                let trimmed = line.trim_start();
                // Skip line comments and doc comments — they're allowed to
                // mention forbidden tokens for documentation purposes.
                if trimmed.starts_with("//") {
                    continue;
                }
                for token in FORBIDDEN_TOKENS {
                    if line.contains(token) {
                        violations.push(format!(
                            "{}:{}: forbidden token `{}` outside client.rs",
                            path.display(),
                            idx + 1,
                            token
                        ));
                    }
                }
            }
        }

        assert!(
            violations.is_empty(),
            "ureq agent / mutation methods must be confined to src/prom/client.rs:\n{}",
            violations.join("\n")
        );
    }

    #[test]
    fn new_strips_trailing_slashes() {
        let c = super::PromClient::new("https://prom:9090/");
        assert_eq!(c.base_url, "https://prom:9090");
        let c = super::PromClient::new("https://prom:9090////");
        assert_eq!(c.base_url, "https://prom:9090");
    }

    #[test]
    fn new_preserves_no_trailing_slash() {
        let c = super::PromClient::new("https://prom:9090");
        assert_eq!(c.base_url, "https://prom:9090");
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
        let resolved = super::resolve_endpoint_inner(
            Some("http://flag:9090"),
            "PROMETHEUS_URL",
            Some("http://env:9090".to_string()),
        )
        .unwrap();
        assert_eq!(resolved, "http://flag:9090");
    }

    #[test]
    fn resolve_endpoint_falls_back_to_env() {
        let resolved = super::resolve_endpoint_inner(
            None,
            "PROMETHEUS_URL",
            Some("http://env:9090".to_string()),
        )
        .unwrap();
        assert_eq!(resolved, "http://env:9090");
    }

    #[test]
    fn resolve_endpoint_errors_when_neither_set() {
        let err = super::resolve_endpoint_inner(None, "PROMETHEUS_URL", None).unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("PROMETHEUS_URL"), "got: {msg}");
        assert!(msg.contains("--url"), "got: {msg}");
    }

    #[test]
    fn resolve_endpoint_treats_empty_flag_as_unset() {
        let resolved = super::resolve_endpoint_inner(
            Some(""),
            "PROMETHEUS_URL",
            Some("http://env:9090".to_string()),
        )
        .unwrap();
        assert_eq!(resolved, "http://env:9090");
    }

    #[test]
    fn resolve_endpoint_treats_empty_env_as_unset() {
        let err =
            super::resolve_endpoint_inner(None, "PROMETHEUS_URL", Some(String::new())).unwrap_err();
        assert!(format!("{err}").contains("PROMETHEUS_URL"));
    }
}
