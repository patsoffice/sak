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

use std::path::PathBuf;

use anyhow::{Context, Result, anyhow, bail};
use http_body_util::{BodyExt, Full};
use hyper::Request;
use hyper::body::Bytes;
use hyper_util::client::legacy::Client;
use hyperlocal::{UnixClientExt, UnixConnector, Uri};

/// A connected Docker client. Bundles the hyper client and the resolved
/// socket path so callers don't have to thread both arguments through every
/// helper.
pub struct DockerClient {
    http: Client<UnixConnector, Full<Bytes>>,
    socket: PathBuf,
}

impl DockerClient {
    /// Build a client by discovering the Docker daemon unix socket.
    ///
    /// See [`discover_socket`] for the search order.
    pub fn connect() -> Result<Self> {
        let socket = discover_socket()?;
        let http: Client<UnixConnector, Full<Bytes>> = Client::unix();
        Ok(Self { http, socket })
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

/// Default system-wide socket path for the Docker daemon.
const DEFAULT_SOCKET: &str = "/var/run/docker.sock";

/// Per-user Docker Desktop socket path, relative to `$HOME`.
///
/// Recent Docker Desktop releases on macOS (and the rootless Linux flavour)
/// expose the engine here instead of `/var/run/docker.sock`, so we have to
/// probe it explicitly.
const USER_SOCKET_SUFFIX: &str = ".docker/run/docker.sock";

/// Discover the Docker daemon unix socket.
///
/// Search order:
/// 1. `DOCKER_HOST` env var if set. Only the `unix://` scheme is supported in
///    v1; `tcp://` (and any other scheme) is rejected with a clear error so
///    the user understands the limitation.
/// 2. `/var/run/docker.sock` (the standard Linux / classic Docker Desktop
///    location).
/// 3. `$HOME/.docker/run/docker.sock` (recent Docker Desktop on macOS, and
///    rootless Linux). Skipped cleanly if `$HOME` is unset.
///
/// Returns an error listing every candidate that was probed if none exist.
pub fn discover_socket() -> Result<PathBuf> {
    if let Ok(env_value) = std::env::var("DOCKER_HOST") {
        return parse_docker_host(&env_value);
    }
    let candidates = socket_candidates(std::env::var("HOME").ok());
    for candidate in &candidates {
        if candidate.exists() {
            return Ok(candidate.clone());
        }
    }
    let searched = candidates
        .iter()
        .map(|p| p.display().to_string())
        .collect::<Vec<_>>()
        .join(", ");
    Err(anyhow!(
        "no Docker daemon unix socket found. Set DOCKER_HOST=unix:///path or \
         start Docker.\nSearched: {searched}"
    ))
}

/// Build the ordered list of unix socket paths to probe when `DOCKER_HOST`
/// is unset. The user-scoped Docker Desktop path is only included when
/// `home` is `Some` and non-empty — `std::env::home_dir` is unstable, so
/// callers resolve `$HOME` themselves and pass it in. Taking `home` as a
/// parameter keeps the function pure and trivially testable without
/// touching process-wide env state.
fn socket_candidates(home: Option<String>) -> Vec<PathBuf> {
    let mut candidates = vec![PathBuf::from(DEFAULT_SOCKET)];
    if let Some(home) = home
        && !home.is_empty()
    {
        candidates.push(PathBuf::from(home).join(USER_SOCKET_SUFFIX));
    }
    candidates
}

/// Parse the value of `DOCKER_HOST` into a unix socket path.
///
/// Accepts `unix:///path/to/socket`. Rejects every other scheme (notably
/// `tcp://`) because v1 of the docker domain is unix-socket only — TCP
/// transport needs cert handling that is out of scope for the foundation.
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

    #[test]
    fn socket_candidates_includes_user_socket_after_default() {
        let candidates = super::socket_candidates(Some("/home/tester".to_string()));
        assert_eq!(
            candidates,
            vec![
                PathBuf::from("/var/run/docker.sock"),
                PathBuf::from("/home/tester/.docker/run/docker.sock"),
            ]
        );
    }

    #[test]
    fn socket_candidates_skips_user_socket_when_home_missing() {
        let candidates = super::socket_candidates(None);
        assert_eq!(candidates, vec![PathBuf::from("/var/run/docker.sock")]);
    }

    #[test]
    fn socket_candidates_skips_user_socket_when_home_empty() {
        let candidates = super::socket_candidates(Some(String::new()));
        assert_eq!(candidates, vec![PathBuf::from("/var/run/docker.sock")]);
    }
}
