//! Sole chokepoint for HTTP access to the Docker Engine REST API.
//!
//! Every other module in `src/docker/` must route HTTP through the helpers
//! exposed here. Importing `hyper::Client`, `hyperlocal::*`, or constructing
//! `hyper::Request::builder()` (or any non-GET method) anywhere else in the
//! domain is forbidden, and the
//! [`tests::no_mutation_methods_outside_client_module`] grep test enforces it
//! on every `cargo test --features docker` run.
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

/// A connected Docker client. Bundles the hyper client and the resolved
/// socket path so callers don't have to thread both arguments through every
/// helper.
//
// `#[allow(dead_code)]` until the first docker subcommand wires it in. The
// foundation issue intentionally adds no commands; dependent issues
// (`list`, `info`, `config`, `images`) consume this helper.
#[allow(dead_code)]
pub struct DockerClient {
    http: Client<UnixConnector, Full<Bytes>>,
    socket: PathBuf,
}

#[allow(dead_code)]
impl DockerClient {
    /// Build a client by discovering the Docker daemon unix socket.
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

    /// Issue a `GET` against a Docker Engine API path (e.g.
    /// `/containers/json?all=true`) and return the parsed JSON body.
    ///
    /// Unlike LXD, Docker does not wrap responses in an envelope — the
    /// returned value is whatever the daemon sent verbatim.
    ///
    /// Returns `Ok(None)` if the daemon responds with 404, so callers can
    /// map "not found" to the sak-standard exit code 1 without losing the
    /// ability to surface other errors as exit code 2 (mirrors the k8s and
    /// lxc domain pattern).
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
            // Docker error responses are JSON of the form `{"message": "..."}`,
            // but surface the raw body so the user sees whatever the daemon
            // said even if the shape changes.
            let snippet = String::from_utf8_lossy(&body);
            bail!("Docker daemon returned HTTP {status} for GET {path}: {snippet}");
        }

        let value: serde_json::Value = serde_json::from_slice(&body)
            .with_context(|| format!("parsing JSON response for {path}"))?;
        Ok(Some(value))
    }
}

/// Default socket path for the Docker daemon.
#[allow(dead_code)]
const DEFAULT_SOCKET: &str = "/var/run/docker.sock";

/// Discover the Docker daemon unix socket.
///
/// Search order:
/// 1. `DOCKER_HOST` env var if set. Only the `unix://` scheme is supported in
///    v1; `tcp://` (and any other scheme) is rejected with a clear error so
///    the user understands the limitation.
/// 2. `/var/run/docker.sock` (the standard Linux/macOS-with-Docker-Desktop
///    location).
///
/// Returns an error describing the candidates if none exist.
#[allow(dead_code)]
pub fn discover_socket() -> Result<PathBuf> {
    if let Ok(env_value) = std::env::var("DOCKER_HOST") {
        return parse_docker_host(&env_value);
    }
    let p = PathBuf::from(DEFAULT_SOCKET);
    if p.exists() {
        return Ok(p);
    }
    Err(anyhow!(
        "no Docker daemon unix socket found. Set DOCKER_HOST=unix:///path or \
         start Docker.\nSearched: {DEFAULT_SOCKET}"
    ))
}

/// Parse the value of `DOCKER_HOST` into a unix socket path.
///
/// Accepts `unix:///path/to/socket`. Rejects every other scheme (notably
/// `tcp://`) because v1 of the docker domain is unix-socket only — TCP
/// transport needs cert handling that is out of scope for the foundation.
#[allow(dead_code)]
fn parse_docker_host(value: &str) -> Result<PathBuf> {
    if let Some(rest) = value.strip_prefix("unix://") {
        if rest.is_empty() {
            bail!("DOCKER_HOST=unix:// has an empty path");
        }
        return Ok(PathBuf::from(rest));
    }
    if value.contains("://") {
        bail!(
            "DOCKER_HOST={value} uses an unsupported scheme. sak's docker \
             domain only supports unix:// sockets."
        );
    }
    // Bare path with no scheme — not standard Docker syntax, but be forgiving
    // and treat it as a unix socket path. Most users will set the standard
    // unix:// form; this just avoids surprising them when they don't.
    Ok(PathBuf::from(value))
}

#[cfg(test)]
mod tests {
    use std::ffi::OsStr;
    use std::fs;
    use std::path::PathBuf;

    /// Tokens that must not appear in any `src/docker/*.rs` file other than
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
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/docker");
        let entries = fs::read_dir(&dir).expect("read src/docker");

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
            "hyper / hyperlocal / mutation methods must be confined to src/docker/client.rs:\n{}",
            violations.join("\n")
        );
    }

    #[test]
    fn parse_docker_host_accepts_unix_scheme() {
        let p = super::parse_docker_host("unix:///var/run/docker.sock").unwrap();
        assert_eq!(p, PathBuf::from("/var/run/docker.sock"));
    }

    #[test]
    fn parse_docker_host_rejects_tcp_scheme() {
        let err = super::parse_docker_host("tcp://127.0.0.1:2375").unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("unsupported scheme"), "got: {msg}");
        assert!(msg.contains("unix://"), "got: {msg}");
    }

    #[test]
    fn parse_docker_host_rejects_empty_unix_path() {
        let err = super::parse_docker_host("unix://").unwrap_err();
        assert!(format!("{err}").contains("empty path"));
    }

    #[test]
    fn parse_docker_host_accepts_bare_path() {
        let p = super::parse_docker_host("/tmp/docker.sock").unwrap();
        assert_eq!(p, PathBuf::from("/tmp/docker.sock"));
    }
}
