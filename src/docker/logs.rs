//! `sak docker logs <container>` — fetch a container's logs.
//!
//! Issues a `GET /containers/<id-or-name>/logs` against the discovered unix
//! socket via [`super::client::DockerClient::get_bytes`] and emits the log
//! lines. Docker multiplexes stdout and stderr into a single response with an
//! 8-byte frame header per chunk *unless* the container was started with a TTY,
//! in which case the stream is raw. We learn which by first inspecting the
//! container's `Config.Tty` — robust across daemon versions, unlike sniffing
//! the response `Content-Type` (the `multiplexed-stream` content type only
//! appears on recent API versions).
//!
//! Buffered, not streaming — same design choice as `sak k8s logs`. The
//! `--tail`/`--since` flags bound the request and `--limit` bounds the output,
//! so buffering the response body is consistent with the other API domains
//! (the prom client buffers whole bodies too).
//!
//! A 404 from the daemon (container not found) maps to exit code 1; any other
//! error is exit code 2.

use std::io;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, anyhow, bail};
use clap::Args;
use regex::Regex;

use crate::docker::client::DockerClient;
use crate::duration::parse_duration;
use crate::output::{BoundedWriter, Outcome};

#[derive(Args)]
#[command(
    about = "Fetch logs from a Docker container",
    long_about = "Fetch the buffered logs of a single Docker container, \
        identified by name or (full or short) ID, and emit them line by line.\n\n\
        Docker multiplexes stdout and stderr into one stream; by default both \
        are emitted unmodified. Pass `--stdout` or `--stderr` to request only \
        one, or `--all-streams` to prefix each line with `[stdout]` / `[stderr]` \
        so the two are distinguishable. (Containers started with a TTY have a \
        single combined stream with no per-line labelling.)\n\n\
        `--since` accepts a compact duration (`30s`, `5m`, `2h30m`, `1d`, `1w`) \
        and is converted to an absolute timestamp relative to now. `--tail` \
        takes a line count or `all`. `--timestamps` asks the daemon to prefix \
        each line with its RFC3339 timestamp. `--grep` filters lines \
        client-side with a regex.\n\n\
        Exit codes: 0 = at least one line emitted, 1 = container not found or \
        no lines after filtering, 2 = error.",
    after_help = "\
Examples:
  sak docker logs web1                              All logs (default tail 100)
  sak docker logs web1 --tail all                   Every line
  sak docker logs web1 --since 10m                  Last 10 minutes
  sak docker logs web1 --stderr                     Only the stderr stream
  sak docker logs web1 --all-streams                Prefix lines with [stdout]/[stderr]
  sak docker logs web1 --timestamps --grep ERROR    Timestamped ERROR lines"
)]
pub struct LogsArgs {
    /// Container name or ID (full or short)
    pub container: String,

    /// Last N lines, or `all` for everything (default: 100)
    #[arg(long, default_value = "100")]
    pub tail: String,

    /// Only logs newer than this duration (e.g. `30s`, `5m`, `2h30m`, `1d`, `1w`)
    #[arg(long)]
    pub since: Option<String>,

    /// Prefix each line with the daemon's RFC3339 timestamp
    #[arg(long)]
    pub timestamps: bool,

    /// Only the stdout stream
    #[arg(long, conflicts_with = "stderr")]
    pub stdout: bool,

    /// Only the stderr stream
    #[arg(long)]
    pub stderr: bool,

    /// Prefix each line with `[stdout]` / `[stderr]`
    #[arg(long)]
    pub all_streams: bool,

    /// Not applicable to Docker — present only to give a helpful error
    #[arg(short = 'p', long, hide = true)]
    pub previous: bool,

    /// Line-oriented regex filter applied client-side
    #[arg(long)]
    pub grep: Option<String>,

    /// Maximum number of output lines
    #[arg(long)]
    pub limit: Option<usize>,
}

/// Which of Docker's multiplexed streams a chunk came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DockerStream {
    Stdout,
    Stderr,
    Other(u8),
}

impl DockerStream {
    fn label(self) -> String {
        match self {
            DockerStream::Stdout => "stdout".to_string(),
            DockerStream::Stderr => "stderr".to_string(),
            DockerStream::Other(o) => format!("stream{o}"),
        }
    }
}

pub async fn run(args: &LogsArgs) -> Result<Outcome> {
    if args.previous {
        bail!("--previous is not supported for docker — Docker keeps no previous-instance log");
    }

    let grep_re = match &args.grep {
        Some(p) => Some(Regex::new(p).with_context(|| format!("invalid --grep regex: {p}"))?),
        None => None,
    };

    // `--stdout` / `--stderr` select a single stream; neither (or both) means
    // both streams.
    let (want_stdout, want_stderr) = match (args.stdout, args.stderr) {
        (true, false) => (true, false),
        (false, true) => (false, true),
        _ => (true, true),
    };

    let since_param = match &args.since {
        Some(s) => {
            let secs = parse_duration(s).map_err(|e| anyhow!("--since: {e}"))?;
            Some(unix_now()?.saturating_sub(secs))
        }
        None => None,
    };

    validate_tail(&args.tail)?;

    let client = DockerClient::connect()?;

    // Inspect first to learn whether the stream is TTY-raw or multiplexed, and
    // to map "no such container" to exit 1 before fetching logs.
    let inspect = client
        .get_json(&format!("/containers/{}/json", args.container))
        .await?;
    let Some(inspect) = inspect else {
        return Ok(Outcome::NotFound);
    };
    let tty = inspect
        .pointer("/Config/Tty")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);

    let path = build_logs_path(
        &args.container,
        want_stdout,
        want_stderr,
        args.timestamps,
        &args.tail,
        since_param,
    );

    let Some(body) = client.get_bytes(&path).await? else {
        return Ok(Outcome::NotFound);
    };

    let lines = lines_from_body(&body, tty)?;

    let stdout = io::stdout();
    let handle = stdout.lock();
    let mut writer = BoundedWriter::new(handle, args.limit);

    let mut wrote_any = false;
    for (stream, line) in lines {
        if let Some(re) = &grep_re
            && !re.is_match(&line)
        {
            continue;
        }
        let out = if args.all_streams {
            format!("[{}] {line}", stream.label())
        } else {
            line
        };
        if !writer.write_line(&out)? {
            break;
        }
        wrote_any = true;
    }

    writer.flush()?;
    if wrote_any {
        Ok(Outcome::Found)
    } else {
        Ok(Outcome::NotFound)
    }
}

/// Seconds since the Unix epoch.
fn unix_now() -> Result<u64> {
    Ok(SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system clock is before the Unix epoch")?
        .as_secs())
}

/// Reject a `--tail` value that is neither `all` nor a non-negative integer.
fn validate_tail(tail: &str) -> Result<()> {
    if tail == "all" || tail.parse::<u64>().is_ok() {
        Ok(())
    } else {
        Err(anyhow!(
            "--tail must be a non-negative integer or `all`, got `{tail}`"
        ))
    }
}

/// Build the `/containers/<name>/logs?...` request path. Pure so the query
/// assembly is unit-testable without a daemon.
fn build_logs_path(
    container: &str,
    stdout: bool,
    stderr: bool,
    timestamps: bool,
    tail: &str,
    since: Option<u64>,
) -> String {
    let mut q = vec![
        format!("stdout={}", if stdout { 1 } else { 0 }),
        format!("stderr={}", if stderr { 1 } else { 0 }),
        format!("timestamps={}", if timestamps { 1 } else { 0 }),
        format!("tail={tail}"),
    ];
    if let Some(since) = since {
        q.push(format!("since={since}"));
    }
    format!("/containers/{container}/logs?{}", q.join("&"))
}

/// Turn a raw logs response body into labelled lines.
///
/// For a TTY container the body is a raw byte stream (treated entirely as
/// stdout). Otherwise Docker frames each write with an 8-byte header
/// (`stream:1, _:3, size:4 big-endian`) — [`demux_frames`] splits those apart.
fn lines_from_body(body: &[u8], tty: bool) -> Result<Vec<(DockerStream, String)>> {
    if tty {
        return Ok(String::from_utf8_lossy(body)
            .lines()
            .map(|l| (DockerStream::Stdout, l.to_string()))
            .collect());
    }
    let mut lines = Vec::new();
    for (stream, payload) in demux_frames(body)? {
        for line in String::from_utf8_lossy(&payload).lines() {
            lines.push((stream, line.to_string()));
        }
    }
    Ok(lines)
}

/// Parse Docker's multiplexed log framing into `(stream, payload)` chunks.
///
/// Each frame is an 8-byte header — byte 0 is the stream type (1 = stdout,
/// 2 = stderr), bytes 1-3 are reserved zeros, bytes 4-7 are the payload length
/// as a big-endian `u32` — followed by exactly that many payload bytes.
fn demux_frames(data: &[u8]) -> Result<Vec<(DockerStream, Vec<u8>)>> {
    let mut out = Vec::new();
    let mut i = 0usize;
    while i < data.len() {
        if i + 8 > data.len() {
            bail!("truncated docker log frame header at byte offset {i}");
        }
        let stream = match data[i] {
            1 => DockerStream::Stdout,
            2 => DockerStream::Stderr,
            other => DockerStream::Other(other),
        };
        let size =
            u32::from_be_bytes([data[i + 4], data[i + 5], data[i + 6], data[i + 7]]) as usize;
        let start = i + 8;
        let end = start
            .checked_add(size)
            .ok_or_else(|| anyhow!("docker log frame size overflows the buffer"))?;
        if end > data.len() {
            bail!(
                "docker log frame at offset {i} claims {size} payload bytes but only {} remain",
                data.len() - start
            );
        }
        out.push((stream, data[start..end].to_vec()));
        i = end;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build one multiplexed frame: header + payload.
    fn frame(stream: u8, payload: &[u8]) -> Vec<u8> {
        let mut v = vec![stream, 0, 0, 0];
        v.extend_from_slice(&(payload.len() as u32).to_be_bytes());
        v.extend_from_slice(payload);
        v
    }

    #[test]
    fn demux_splits_stdout_and_stderr_frames() {
        let mut data = frame(1, b"out line\n");
        data.extend(frame(2, b"err line\n"));
        let frames = demux_frames(&data).unwrap();
        assert_eq!(
            frames,
            vec![
                (DockerStream::Stdout, b"out line\n".to_vec()),
                (DockerStream::Stderr, b"err line\n".to_vec()),
            ]
        );
    }

    #[test]
    fn demux_rejects_truncated_header() {
        // Only 5 bytes — not a full 8-byte header.
        let data = vec![1, 0, 0, 0, 9];
        assert!(demux_frames(&data).is_err());
    }

    #[test]
    fn demux_rejects_oversized_frame() {
        // Header claims 100 payload bytes but the buffer has none.
        let mut data = vec![1u8, 0, 0, 0];
        data.extend_from_slice(&100u32.to_be_bytes());
        assert!(demux_frames(&data).is_err());
    }

    #[test]
    fn lines_from_multiplexed_body() {
        let mut data = frame(1, b"a\nb\n");
        data.extend(frame(2, b"c\n"));
        let lines = lines_from_body(&data, false).unwrap();
        assert_eq!(
            lines,
            vec![
                (DockerStream::Stdout, "a".to_string()),
                (DockerStream::Stdout, "b".to_string()),
                (DockerStream::Stderr, "c".to_string()),
            ]
        );
    }

    #[test]
    fn lines_from_tty_body_is_raw() {
        // No frame headers — the whole thing is one raw stdout stream.
        let lines = lines_from_body(b"line1\nline2\n", true).unwrap();
        assert_eq!(
            lines,
            vec![
                (DockerStream::Stdout, "line1".to_string()),
                (DockerStream::Stdout, "line2".to_string()),
            ]
        );
    }

    #[test]
    fn build_logs_path_includes_all_params() {
        let p = build_logs_path("web1", true, false, true, "50", Some(1700000000));
        assert_eq!(
            p,
            "/containers/web1/logs?stdout=1&stderr=0&timestamps=1&tail=50&since=1700000000"
        );
    }

    #[test]
    fn build_logs_path_omits_since_when_absent() {
        let p = build_logs_path("web1", true, true, false, "all", None);
        assert_eq!(
            p,
            "/containers/web1/logs?stdout=1&stderr=1&timestamps=0&tail=all"
        );
    }

    #[test]
    fn validate_tail_accepts_all_and_integers() {
        assert!(validate_tail("all").is_ok());
        assert!(validate_tail("0").is_ok());
        assert!(validate_tail("100").is_ok());
    }

    #[test]
    fn validate_tail_rejects_garbage() {
        assert!(validate_tail("-5").is_err());
        assert!(validate_tail("lots").is_err());
        assert!(validate_tail("").is_err());
    }
}
