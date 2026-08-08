use std::io::{self, StdoutLock, Write};
use std::process::ExitCode;

/// Typed result of a command's `run()` — replaces bare [`ExitCode`] integers.
///
/// The found / not-found / partial axis is matchable and testable, and the
/// magic exit-1 (negative-result) is named rather than spelled `ExitCode::from(1)`.
/// Errors stay in the `Err` arm of `anyhow::Result<Outcome>` and continue to
/// map to exit code 2 via [`main`](crate::main)'s error renderer.
///
/// Two commands intentionally invert the found/not-found mapping (`sak cert
/// expiring`, `sak helm lint`) — see those modules for the spec-inversion
/// comments. They return the variant whose [`Outcome::exit_code`] matches the
/// desired shell-script semantics, not the literal "did we find something".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// Operation succeeded and produced at least one result. Exit 0.
    Found,
    /// Operation succeeded with no results (negative result, not an error).
    /// Exit 1.
    NotFound,
    /// Operation produced partial output to stdout, but at least one item
    /// hit a per-item error already reported on stderr. Exit 2.
    ///
    /// Used by [`fs::stat`](crate::fs::stat) and [`fs::wc`](crate::fs::wc),
    /// which keep going across the input set even when individual files fail.
    /// Distinct from `Err(...)` because the latter aborts the run before any
    /// output reaches stdout.
    Partial,
}

impl Outcome {
    /// Map an [`Outcome`] to its process exit code.
    pub fn exit_code(self) -> ExitCode {
        match self {
            Outcome::Found => ExitCode::SUCCESS,
            Outcome::NotFound => ExitCode::from(1),
            Outcome::Partial => ExitCode::from(2),
        }
    }
}

/// Single-fetch helper: turn an `Option<T>` from a lookup into an [`Outcome`],
/// rendering the value via the supplied closure when present.
///
/// Used by commands that fetch one named resource and emit its representation
/// (e.g. `sak docker info`, `sak lxc info`) — `None` from the foundation
/// chokepoint (`get_json`) cleanly becomes [`Outcome::NotFound`] without the
/// caller writing a `match` ladder around `Ok(Some(_))` / `Ok(None)`.
///
/// The closure is synchronous, so this fits the sync-render-after-async-fetch
/// shape (lxc/docker info, prom). Commands whose render body is also async —
/// e.g. `sak k8s describe`, which interleaves `write_*_section(...).await?`
/// calls — keep the explicit `let Some(...) = ... else { return ... }` pattern.
///
/// Always on (no cfg gate) so sync API domains like `prom` and future call
/// sites can adopt it without dragging in a docker/lxc cargo feature. The
/// `allow(dead_code)` covers the lean `--no-default-features` build, where no
/// adopting domain is compiled in.
#[allow(dead_code)]
pub fn outcome_from_option<T>(
    value: Option<T>,
    render: impl FnOnce(T) -> anyhow::Result<()>,
) -> anyhow::Result<Outcome> {
    match value {
        Some(v) => {
            render(v)?;
            Ok(Outcome::Found)
        }
        None => Ok(Outcome::NotFound),
    }
}

/// How a [`BoundedWriter`]'s output cap was chosen. Controls whether hitting
/// the cap is announced on stderr — see [`truncation_notice`].
///
/// Nearly every command's `--limit` is an `Option<usize>` that is `None` unless
/// the caller passes the flag, so those call sites convert straight from
/// `Option<usize>` via [`From`]. Commands whose limit has a built-in default
/// (`sak fs read`'s 2000 lines) must name [`Limit::Default`] explicitly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Limit {
    /// No cap — write everything.
    None,
    /// The caller asked for this cap with `--limit N`.
    Explicit(usize),
    /// The cap is a command's built-in default; the caller never chose it.
    Default(usize),
}

impl Limit {
    /// The number of countable lines allowed, or `None` when unbounded.
    fn max_lines(self) -> Option<usize> {
        match self {
            Limit::None => None,
            Limit::Explicit(n) | Limit::Default(n) => Some(n),
        }
    }
}

impl From<Option<usize>> for Limit {
    fn from(limit: Option<usize>) -> Self {
        match limit {
            Some(n) => Limit::Explicit(n),
            None => Limit::None,
        }
    }
}

/// The stderr note to emit the first time output hits `limit`, or `None` when
/// truncation needs no announcement.
///
/// A caller who passed `--limit N` got exactly the cut they asked for, so the
/// note is pure noise — and a `sak:` line on stderr in an otherwise-successful
/// run reads like a failure, which trains callers to ignore the `sak:` lines
/// that *do* matter (sak-llm-cli-exit-codes-p209). A cap that came from a
/// command's built-in default is not the caller's choice, so silently handing
/// back a partial answer would be worse than the noise — that case still
/// announces itself, and says how to raise the cap.
///
/// Truncation is never an error either way: the run still exits 0.
fn truncation_notice(limit: Limit) -> Option<String> {
    match limit {
        Limit::Default(n) => Some(format!(
            "sak: output truncated at {n} results (default limit — pass --limit to raise it)"
        )),
        Limit::Explicit(_) | Limit::None => None,
    }
}

/// A writer that writes to stdout and tracks lines written, stopping at a
/// configurable [`Limit`]. Reaching a *default* limit writes a notice to
/// stderr; reaching a caller-supplied one is silent.
pub struct BoundedWriter<'a> {
    inner: StdoutLock<'a>,
    limit: Limit,
    lines_written: usize,
    truncated: bool,
}

impl<'a> BoundedWriter<'a> {
    pub fn new(inner: StdoutLock<'a>, limit: impl Into<Limit>) -> Self {
        Self {
            inner,
            limit: limit.into(),
            lines_written: 0,
            truncated: false,
        }
    }

    /// Write a single line (including newline). Returns Ok(true) if the line
    /// was written, Ok(false) if the limit has been reached.
    pub fn write_line(&mut self, line: &str) -> io::Result<bool> {
        if let Some(limit) = self.limit.max_lines()
            && self.lines_written >= limit
        {
            if !self.truncated {
                self.truncated = true;
                if let Some(notice) = truncation_notice(self.limit) {
                    eprintln!("{notice}");
                }
            }
            return Ok(false);
        }
        self.inner.write_all(line.as_bytes())?;
        if !line.ends_with('\n') {
            self.inner.write_all(b"\n")?;
        }
        self.lines_written += 1;
        Ok(true)
    }

    /// Write a line without counting it toward the limit (for separators, headings, etc.)
    pub fn write_decoration(&mut self, line: &str) -> io::Result<()> {
        if self.truncated {
            return Ok(());
        }
        self.inner.write_all(line.as_bytes())?;
        if !line.ends_with('\n') {
            self.inner.write_all(b"\n")?;
        }
        Ok(())
    }

    pub fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

/// Format a line number right-aligned to the given width, followed by a tab.
pub fn format_line_number(n: usize, width: usize) -> String {
    format!("{:>width$}\t", n, width = width)
}

/// Compute the display width needed for line numbers up to `max`.
pub fn line_number_width(max: usize) -> usize {
    if max == 0 {
        1
    } else {
        ((max as f64).log10().floor() as usize) + 1
    }
}

/// Convert a path to a display string relative to a base path.
pub fn relative_path(path: &std::path::Path, base: &std::path::Path) -> String {
    path.strip_prefix(base)
        .unwrap_or(path)
        .to_string_lossy()
        .into_owned()
}

/// Check if a file appears to be binary by looking for NUL bytes in the first 8KB.
pub fn is_binary(path: &std::path::Path) -> io::Result<bool> {
    use std::io::Read;
    let mut file = std::fs::File::open(path)?;
    let mut buf = [0u8; 8192];
    let n = file.read(&mut buf)?;
    Ok(buf[..n].contains(&0))
}

/// Collapse `\n` and `\r` in `s` to single spaces so a multi-line free-text
/// field stays on one output row. Used by the row-oriented emitters in `k8s`
/// (event/describe messages) and `prom` (alert summaries, target `lastError`,
/// rule queries, ...). Tabs are left intact — those domains don't emit
/// tab-delimited rows whose contract a stray `\t` would break.
///
/// Implemented via `chars().map()` rather than `str::replace` because the
/// k8s chokepoint grep test forbids `.replace(` outside `client.rs` (it would
/// also catch `kube::Api::replace`, the mutation method being guarded).
///
/// Gated to the domains that use it so lean builds don't trip the dead-code
/// lint. See [`collapse_ws`] for the variant that also collapses tabs.
#[cfg(any(feature = "k8s", feature = "prom"))]
pub fn collapse_newlines(s: &str) -> String {
    s.chars()
        .map(|c| if c == '\n' || c == '\r' { ' ' } else { c })
        .collect()
}

/// Collapse `\n`, `\r`, and `\t` in `s` to single spaces so a value can't
/// break a one-record-per-line, tab-delimited (TSV) contract. Used by the
/// `gh` domain, whose `--fields` output is genuinely tab-separated. See
/// [`collapse_newlines`] for the newline-only variant. Implemented via
/// `chars().map()` to match the repo's chokepoint conventions.
pub fn collapse_ws(s: &str) -> String {
    s.chars()
        .map(|c| {
            if matches!(c, '\n' | '\r' | '\t') {
                ' '
            } else {
                c
            }
        })
        .collect()
}

/// Pretty-print `data` as JSON through a [`BoundedWriter`], honoring `--limit`.
/// This is the `--json` / metadata-dump branch shared by `sak prom` (every
/// command), `sak k8s schema`, `sak docker info`, and `sak lxc info`. Always
/// returns [`Outcome::Found`] — a JSON dump of an empty result is still a
/// successful response, just an empty one.
///
/// Gated to the domains that use it so lean builds don't carry it (and don't
/// trip the dead-code lint).
#[cfg(any(feature = "k8s", feature = "lxc", feature = "docker", feature = "prom"))]
pub fn emit_json(data: &serde_json::Value, limit: Option<usize>) -> anyhow::Result<Outcome> {
    let stdout = io::stdout();
    let handle = stdout.lock();
    let mut writer = BoundedWriter::new(handle, limit);
    let pretty = serde_json::to_string_pretty(data)?;
    for line in pretty.lines() {
        if !writer.write_line(line)? {
            break;
        }
    }
    writer.flush()?;
    Ok(Outcome::Found)
}

/// Build a current-thread tokio runtime and block on `fut`, returning its
/// result. Shared by the async domains (`k8s`, `lxc`, `docker`) so each
/// domain's `run()` doesn't repeat the runtime-bootstrap boilerplate. The
/// runtime is dropped before this returns, so the rest of sak stays sync.
///
/// Gated to the features that actually pull in tokio, so lean builds
/// (`--no-default-features`) never see it.
#[cfg(any(feature = "k8s", feature = "lxc", feature = "docker"))]
pub fn run_async<F, T>(fut: F) -> anyhow::Result<T>
where
    F: std::future::Future<Output = anyhow::Result<T>>,
{
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    rt.block_on(fut)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_line_number() {
        assert_eq!(format_line_number(1, 4), "   1\t");
        assert_eq!(format_line_number(42, 4), "  42\t");
        assert_eq!(format_line_number(1000, 4), "1000\t");
    }

    #[test]
    fn test_line_number_width() {
        assert_eq!(line_number_width(1), 1);
        assert_eq!(line_number_width(9), 1);
        assert_eq!(line_number_width(10), 2);
        assert_eq!(line_number_width(99), 2);
        assert_eq!(line_number_width(100), 3);
        assert_eq!(line_number_width(999), 3);
        assert_eq!(line_number_width(1000), 4);
    }

    #[test]
    fn test_relative_path() {
        use std::path::Path;
        let base = Path::new("/home/user/project");
        let full = Path::new("/home/user/project/src/main.rs");
        assert_eq!(relative_path(full, base), "src/main.rs");

        let other = Path::new("/tmp/file.txt");
        assert_eq!(relative_path(other, base), "/tmp/file.txt");
    }

    #[cfg(any(feature = "k8s", feature = "prom"))]
    #[test]
    fn collapse_newlines_replaces_cr_and_lf_but_keeps_tabs() {
        assert_eq!(
            collapse_newlines("line1\nline2\rline3\r\nline4"),
            "line1 line2 line3  line4"
        );
        assert_eq!(collapse_newlines("no newlines here"), "no newlines here");
        // tabs are deliberately preserved by the newline-only variant
        assert_eq!(collapse_newlines("a\tb"), "a\tb");
    }

    #[test]
    fn truncation_is_announced_only_for_default_limits() {
        // A cap the caller asked for is not news; a built-in one is, and the
        // notice has to say how to raise it.
        assert_eq!(truncation_notice(Limit::Explicit(10)), None);
        assert_eq!(truncation_notice(Limit::None), None);
        let notice = truncation_notice(Limit::Default(2000)).expect("default limit announces");
        assert!(notice.contains("2000"), "{notice}");
        assert!(notice.contains("--limit"), "{notice}");
        // Must not read as a failure — callers key off the `sak: error:` prefix.
        assert!(!notice.contains("error"), "{notice}");
    }

    #[test]
    fn bare_option_limits_are_treated_as_caller_supplied() {
        assert_eq!(Limit::from(Some(5)), Limit::Explicit(5));
        assert_eq!(Limit::from(None), Limit::None);
    }

    #[test]
    fn collapse_ws_replaces_newlines_and_tabs() {
        assert_eq!(
            collapse_ws("line1\nline2\twith tab\rline3"),
            "line1 line2 with tab line3"
        );
        assert_eq!(
            collapse_ws("no whitespace controls"),
            "no whitespace controls"
        );
    }
}
