//! Sole chokepoint for HTTP access to the LXD / Incus REST API.
//!
//! Every other module in `src/lxc/` must route HTTP through the helpers
//! exposed here. Importing `hyper::Client`, `hyperlocal::*`, or constructing
//! `hyper::Request::builder()` (or any non-GET method) anywhere else in the
//! domain is forbidden, and the
//! [`tests::no_mutation_methods_outside_client_module`] grep test enforces it
//! on every `cargo test --features lxc` run.
//!
//! `hyper` does not provide a read-only client variant, so this convention is
//! the only thing that keeps the domain provably free of writes.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use http_body_util::{BodyExt, Full};
use hyper::Request;
use hyper::body::Bytes;
use hyper_util::client::legacy::Client;
use hyperlocal::{UnixClientExt, UnixConnector, Uri};

/// A connected LXD/Incus client. Bundles the hyper client and the resolved
/// socket path so callers don't have to thread both arguments through every
/// helper.
//
// `#[allow(dead_code)]` until the first lxc subcommand wires it in. The
// foundation issue intentionally adds no commands; dependent issues
// (`list`, `info`, `config`, `images`) consume this helper.
#[allow(dead_code)]
pub struct LxcClient {
    http: Client<UnixConnector, Full<Bytes>>,
    socket: PathBuf,
}

#[allow(dead_code)]
impl LxcClient {
    /// Build a client by discovering the LXD/Incus unix socket on disk.
    ///
    /// See [`discover_socket`] for the search order.
    pub fn connect() -> Result<Self> {
        let socket = discover_socket()?;
        let http: Client<UnixConnector, Full<Bytes>> = Client::unix();
        Ok(Self { http, socket })
    }

    /// The socket path this client is talking to. Useful for error messages.
    pub fn socket(&self) -> &Path {
        &self.socket
    }

    /// Issue a `GET` against an LXD path (e.g. `/1.0/instances`), parse the
    /// LXD response envelope `{type, status, status_code, metadata}`, and
    /// return the `metadata` field.
    ///
    /// Returns `Ok(None)` if the apiserver responds with 404, so callers can
    /// map "not found" to the sak-standard exit code 1 without losing the
    /// ability to surface other errors as exit code 2 (mirrors the k8s domain
    /// pattern in `src/k8s/client.rs`).
    pub async fn get_json(&self, path: &str) -> Result<Option<serde_json::Value>> {
        let uri: hyper::Uri = Uri::new(&self.socket, path).into();
        let req = Request::builder()
            .method("GET")
            .uri(uri)
            .header("Host", "localhost")
            .body(Full::<Bytes>::default())
            .with_context(|| format!("building request for {path}"))?;

        let res = self
            .http
            .request(req)
            .await
            .with_context(|| format!("GET {path} via {}", self.socket.display()))?;

        let status = res.status();
        let body = res
            .into_body()
            .collect()
            .await
            .with_context(|| format!("reading response body for {path}"))?
            .to_bytes();

        if status.as_u16() == 404 {
            return Ok(None);
        }
        if !status.is_success() {
            // LXD error responses are also envelopes; surface the body verbatim
            // so the user sees whatever the daemon said.
            let snippet = String::from_utf8_lossy(&body);
            bail!("LXD returned HTTP {status} for GET {path}: {snippet}");
        }

        let envelope: serde_json::Value = serde_json::from_slice(&body)
            .with_context(|| format!("parsing JSON response for {path}"))?;

        // LXD wraps every successful response in
        //   {"type": "sync"|"async", "status": "Success", "status_code": 200,
        //    "metadata": <payload>, ...}
        // We unwrap to `metadata`. If the envelope is missing (which would be
        // a protocol violation) surface a clear error rather than silently
        // returning the whole envelope.
        let metadata = envelope
            .get("metadata")
            .ok_or_else(|| anyhow!("LXD response for {path} has no `metadata` field"))?
            .clone();
        Ok(Some(metadata))
    }

    /// Convenience wrapper for `?recursion=N`.
    ///
    /// Appends the query parameter to `path`, preserving any existing query
    /// string.
    pub async fn get_json_recursive(
        &self,
        path: &str,
        recursion: u8,
    ) -> Result<Option<serde_json::Value>> {
        let sep = if path.contains('?') { '&' } else { '?' };
        let with_recursion = format!("{path}{sep}recursion={recursion}");
        self.get_json(&with_recursion).await
    }
}

/// Default search path order for the LXD/Incus unix socket.
#[allow(dead_code)]
const SOCKET_CANDIDATES: &[&str] = &[
    "/var/snap/lxd/common/lxd/unix.socket",
    "/var/lib/lxd/unix.socket",
    "/var/lib/incus/unix.sock",
];

/// Discover the LXD/Incus unix socket.
///
/// Search order:
/// 1. `LXD_SOCKET` env var (used as-is, no validation),
/// 2. `/var/snap/lxd/common/lxd/unix.socket` (snap install),
/// 3. `/var/lib/lxd/unix.socket` (deb install),
/// 4. `/var/lib/incus/unix.sock` (Incus fork).
///
/// Returns an error listing all candidates if none exist.
#[allow(dead_code)]
pub fn discover_socket() -> Result<PathBuf> {
    if let Ok(env_path) = std::env::var("LXD_SOCKET") {
        return Ok(PathBuf::from(env_path));
    }
    for candidate in SOCKET_CANDIDATES {
        let p = PathBuf::from(candidate);
        if p.exists() {
            return Ok(p);
        }
    }
    Err(anyhow!(
        "no LXD/Incus unix socket found. Set LXD_SOCKET or install LXD/Incus.\n\
         Searched: {}",
        SOCKET_CANDIDATES.join(", ")
    ))
}

#[cfg(test)]
mod tests {
    use std::ffi::OsStr;
    use std::fs;
    use std::path::PathBuf;

    /// Tokens that must not appear in any `src/lxc/*.rs` file other than
    /// `client.rs`. Comments are exempt — the skip logic below ignores any
    /// line whose first non-whitespace characters are `//`.
    const FORBIDDEN_TOKENS: &[&str] = &[
        "hyper::Client",
        "hyper::client",
        "hyperlocal::",
        "Method::POST",
        "Method::PUT",
        "Method::PATCH",
        "Method::DELETE",
        "Request::post",
        "Request::put",
        "Request::patch",
        "Request::delete",
        "Request::builder",
    ];

    #[test]
    fn no_mutation_methods_outside_client_module() {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/lxc");
        let entries = fs::read_dir(&dir).expect("read src/lxc");

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
            "hyper / hyperlocal / mutation methods must be confined to src/lxc/client.rs:\n{}",
            violations.join("\n")
        );
    }
}
