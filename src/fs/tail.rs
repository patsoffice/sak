use crate::output::Outcome;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use clap::Args;

use super::read::{ReadArgs, run as read_run};
use super::{is_stdin, require_piped_stdin};

#[derive(Args)]
#[command(
    about = "Show the last N lines (or bytes) of a file",
    long_about = "Show the last N lines of a file (default 10).\n\n\
        An ergonomic shorthand for `sak fs read <file> -n -N`: line numbers are \
        on by default and reflect the lines' real positions in the file (disable \
        with --no-line-numbers). Pass --bytes to emit the last N bytes raw \
        instead of whole lines (the byte output is written verbatim, with no \
        line numbers). With the file omitted, reads piped stdin (exit 2 if \
        nothing is piped); '-' always reads stdin (matches cat/grep). Stdin \
        can't be seeked, so it is read \
        fully into memory.\n\n\
        --follow prints the last N and then polls the file every 250ms for \
        appended data, continuing line numbers; a truncated file restarts from \
        offset 0 with numbering reset. It stops after --for DURATION (default \
        30s; `--for 0` follows until killed) and exits 0 if new data arrived \
        in that window, 1 if not — so a call never hangs an agent. --follow \
        on stdin has no effect (stdin drains to EOF and exits).",
    after_help = "\
Examples:
  sak fs tail logfile.txt                  Last 10 lines
  sak fs tail logfile.txt 50               Last 50 lines
  sak fs tail -n 50 logfile.txt            Last 50 lines (-n spelling)
  sak fs tail --no-line-numbers file.txt   Last 10 lines, no line numbers
  sak fs tail --bytes 256 capture.bin      Last 256 bytes (raw)
  some-cmd | sak fs tail                   Last 10 lines of piped stdin
  some-cmd | sak fs tail 5                 Last 5 lines of piped stdin
  sak fs tail --follow app.log             Last 10 lines, then follow for 30s
  sak fs tail --follow --for 5m app.log    Follow for five minutes
  sak fs tail --follow --for 0 app.log     Follow until killed"
)]
pub struct TailArgs {
    /// Path to the file to read (omitted: piped stdin; '-' always reads stdin)
    pub file: Option<PathBuf>,

    /// Number of lines to show (default 10)
    #[arg(value_name = "N", conflicts_with_all = ["bytes", "num_lines"])]
    pub lines: Option<usize>,

    /// Number of lines to show (coreutils `-n` spelling; conflicts with positional N)
    #[arg(short = 'n', long = "lines", value_name = "N", conflicts_with_all = ["lines", "bytes"])]
    pub num_lines: Option<usize>,

    /// Show the last N bytes instead of lines (raw output)
    #[arg(long, value_name = "N")]
    pub bytes: Option<usize>,

    /// Omit line numbers from output
    #[arg(long = "no-line-numbers")]
    pub no_line_numbers: bool,

    /// Print the last N and then poll for appended data (files only)
    #[arg(long)]
    pub follow: bool,

    /// How long --follow polls (e.g. 30s, 5m; default 30s; 0 = until killed)
    #[arg(long = "for", value_name = "DURATION", requires = "follow")]
    pub follow_for: Option<String>,
}

/// `--follow`'s window when `--for` is omitted: long enough to catch a
/// burst of log lines, short enough that an agent never hangs on it.
const DEFAULT_FOLLOW_WINDOW: &str = "30s";

/// Parse `--for`: a compact duration (`30s`, `5m`, `1h30m`), or `0` for no
/// limit (returned as `None`).
fn follow_window(spec: Option<&str>) -> Result<Option<Duration>> {
    let spec = spec.unwrap_or(DEFAULT_FOLLOW_WINDOW);
    if spec == "0" {
        return Ok(None);
    }
    let secs = crate::duration::parse_duration(spec)
        .with_context(|| format!("invalid --for duration: {spec}"))?;
    Ok(Some(Duration::from_secs(secs)))
}

pub fn run(args: &TailArgs) -> Result<Outcome> {
    if let Some(n) = args.bytes {
        return match args.file.as_ref() {
            // Omitted file reads piped stdin; '-' always reads stdin (a
            // drained pipe has no growth to follow, so --follow is a no-op).
            None => {
                require_piped_stdin()?;
                tail_bytes_stdin(n)
            }
            Some(path) if is_stdin(path) => tail_bytes_stdin(n),
            Some(path) => {
                if args.follow {
                    let window = follow_window(args.follow_for.as_deref())?;
                    follow_bytes(path, n, window, &mut io::stdout().lock())
                } else {
                    tail_bytes(path, n)
                }
            }
        };
    }
    // `-n` and the positional N conflict in clap; a lone all-digit positional
    // with no such file reads as a count over stdin (`cmd | sak fs tail 5`).
    let (file, count) =
        super::resolve_file_and_count(args.file.clone(), args.lines, args.num_lines);
    let n = count.unwrap_or(10);
    if args.follow
        && let Some(path) = file.as_ref()
        && !is_stdin(path)
    {
        let window = follow_window(args.follow_for.as_deref())?;
        return follow_lines(
            path,
            n,
            args.no_line_numbers,
            window,
            &mut io::stdout().lock(),
        );
    }
    // Delegate the line path to `read`'s last-N handling so tail stays a thin,
    // consistent wrapper (and line numbers reflect real file positions).
    // (--follow on stdin drains to EOF and exits: a pipe has nothing to poll.)
    let read_args = ReadArgs {
        file,
        lines: Some(format!("-{n}")),
        no_line_numbers: args.no_line_numbers,
        // Explicit (not `read`'s built-in cap), so hitting it is silent —
        // `tail` truncating to n lines is the whole point of the command.
        limit: Some(n),
        offset: 0,
    };
    read_run(&read_args)
}

/// Poll interval for `--follow`: no inotify dependency, so growth is polled.
const FOLLOW_POLL: Duration = Duration::from_millis(250);

/// What one `--follow` poll found.
enum Growth<'a> {
    /// Bytes appended since the last poll.
    Appended(&'a [u8]),
    /// The file shrank (truncated or rotated in place); reading restarts at 0.
    Truncated,
}

/// Poll `path` for growth past `offset` every [`FOLLOW_POLL`] until `window`
/// elapses (`None`: forever), handing each change to `on_growth`. Returns
/// whether any data was appended. Shared by the line and byte modes.
fn poll_growth(
    path: &PathBuf,
    mut offset: u64,
    window: Option<Duration>,
    mut on_growth: impl FnMut(Growth<'_>) -> Result<()>,
) -> Result<bool> {
    let mut f =
        std::fs::File::open(path).with_context(|| format!("cannot open: {}", path.display()))?;
    let deadline = window.map(|w| Instant::now() + w);
    let mut grew = false;
    loop {
        let nap = match deadline {
            Some(d) => {
                let left = d.saturating_duration_since(Instant::now());
                if left.is_zero() {
                    return Ok(grew);
                }
                left.min(FOLLOW_POLL)
            }
            None => FOLLOW_POLL,
        };
        std::thread::sleep(nap);
        let len = f
            .metadata()
            .with_context(|| format!("cannot stat: {}", path.display()))?
            .len();
        if len < offset {
            offset = 0;
            on_growth(Growth::Truncated)?;
            continue;
        }
        if len == offset {
            continue;
        }
        f.seek(SeekFrom::Start(offset))
            .with_context(|| format!("error seeking: {}", path.display()))?;
        let mut buf = Vec::new();
        f.read_to_end(&mut buf)
            .with_context(|| format!("error reading: {}", path.display()))?;
        offset += buf.len() as u64;
        grew = true;
        on_growth(Growth::Appended(&buf))?;
    }
}

/// Print the last `n` lines of `file`, then follow appended lines for
/// `window`. Line numbers continue across polls; a trailing partial line
/// completes with its original number. Exit 0 if new data arrived.
fn follow_lines(
    path: &PathBuf,
    n: usize,
    no_line_numbers: bool,
    window: Option<Duration>,
    out: &mut impl Write,
) -> Result<Outcome> {
    // Bytes, not `read_to_string`: a log with a stray invalid byte must still
    // follow (lines render lossily, as the poll path already does), and the
    // poll offset must count the file's real bytes.
    let bytes = std::fs::read(path).with_context(|| format!("cannot read: {}", path.display()))?;
    let content = String::from_utf8_lossy(&bytes);
    let (initial, mut pending, mut next_line) = follow_initial(&content, n);
    emit_numbered(out, &initial, no_line_numbers)?;
    out.flush()?;

    let grew = poll_growth(path, bytes.len() as u64, window, |growth| {
        match growth {
            Growth::Truncated => {
                pending.clear();
                next_line = 1;
            }
            Growth::Appended(buf) => {
                pending.extend_from_slice(buf);
                for line in take_complete_lines(&mut pending) {
                    emit_numbered(out, &[(next_line, line)], no_line_numbers)?;
                    next_line += 1;
                }
                out.flush()?;
            }
        }
        Ok(())
    })?;
    Ok(if grew {
        Outcome::Found
    } else {
        Outcome::NotFound
    })
}

/// Split `--follow` startup state out of current content: the last-`n` lines
/// to print now as `(line_number, body)`, a buffered partial trailing line
/// (empty when the content ends with `\n`), and the number the next completed
/// line takes.
fn follow_initial(content: &str, n: usize) -> (Vec<(usize, String)>, Vec<u8>, usize) {
    let lines: Vec<&str> = content.lines().collect();
    let total = lines.len();
    let start = total.saturating_sub(n);
    let initial = lines[start..]
        .iter()
        .enumerate()
        .map(|(i, l)| (start + i + 1, l.to_string()))
        .collect();
    let mut pending = Vec::new();
    let mut next_line = total + 1;
    if !content.ends_with('\n')
        && let Some(last) = lines.last()
    {
        // A trailing partial line was just printed as `total`; its completion
        // reuses that number rather than advancing past it.
        pending.extend_from_slice(last.as_bytes());
        next_line = total;
    }
    (initial, pending, next_line)
}

/// Drain complete (`\n`-terminated, lossy-UTF-8) lines off the front of
/// `pending`, leaving any partial trailing line buffered.
fn take_complete_lines(pending: &mut Vec<u8>) -> Vec<String> {
    let mut out = Vec::new();
    let mut begin = 0;
    for (i, &b) in pending.iter().enumerate() {
        if b == b'\n' {
            out.push(String::from_utf8_lossy(&pending[begin..i]).into_owned());
            begin = i + 1;
        }
    }
    pending.drain(..begin);
    out
}

/// Write `(line_number, body)` rows, numbered like `read` unless disabled.
fn emit_numbered(
    out: &mut impl Write,
    rows: &[(usize, String)],
    no_line_numbers: bool,
) -> Result<()> {
    // Width from the largest number on screen; later numbers may exceed it,
    // which format_line_number tolerates by printing the full number.
    let width = crate::output::line_number_width(rows.iter().map(|(n, _)| *n).max().unwrap_or(0));
    for (num, body) in rows {
        if no_line_numbers {
            writeln!(out, "{body}")?;
        } else {
            writeln!(
                out,
                "{}{body}",
                crate::output::format_line_number(*num, width)
            )?;
        }
    }
    Ok(())
}

/// Print the last `n` bytes of `file` raw, then follow appended bytes for
/// `window`. Exit 0 if new data arrived.
fn follow_bytes(
    path: &PathBuf,
    n: usize,
    window: Option<Duration>,
    out: &mut impl Write,
) -> Result<Outcome> {
    tail_bytes(path, n)?;
    let offset = std::fs::metadata(path)
        .with_context(|| format!("cannot stat: {}", path.display()))?
        .len();
    let grew = poll_growth(path, offset, window, |growth| {
        if let Growth::Appended(buf) = growth {
            out.write_all(buf)?;
            out.flush()?;
        }
        Ok(())
    })?;
    Ok(if grew {
        Outcome::Found
    } else {
        Outcome::NotFound
    })
}

/// Write the last `n` bytes of `file` raw to stdout (byte-faithful).
fn tail_bytes(file: &PathBuf, n: usize) -> Result<Outcome> {
    let mut f =
        std::fs::File::open(file).with_context(|| format!("cannot open: {}", file.display()))?;
    let len = f
        .metadata()
        .with_context(|| format!("cannot stat: {}", file.display()))?
        .len();
    let n = (n as u64).min(len);
    if n == 0 {
        return Ok(Outcome::NotFound);
    }
    f.seek(SeekFrom::Start(len - n))
        .with_context(|| format!("error seeking: {}", file.display()))?;
    let mut buf = Vec::with_capacity(n.min(64 * 1024) as usize);
    f.read_to_end(&mut buf)
        .with_context(|| format!("error reading: {}", file.display()))?;
    let stdout = io::stdout();
    stdout.lock().write_all(&buf)?;
    Ok(Outcome::Found)
}

/// Write the last `n` bytes of stdin raw to stdout (byte-faithful). Stdin can't
/// be seeked, so the whole stream is read into memory before the last `n` bytes
/// are sliced off — the same tradeoff `tail`/`sed` make on a pipe.
fn tail_bytes_stdin(n: usize) -> Result<Outcome> {
    let mut buf = Vec::new();
    io::stdin()
        .lock()
        .read_to_end(&mut buf)
        .context("error reading input")?;
    let tail = last_bytes(&buf, n);
    if tail.is_empty() {
        return Ok(Outcome::NotFound);
    }
    let stdout = io::stdout();
    stdout.lock().write_all(tail)?;
    Ok(Outcome::Found)
}

/// The last `n` bytes of `buf`, clamped to the whole buffer when `n` exceeds
/// its length. Mirrors the file path's `min(len)` clamp for the un-seekable
/// stdin case.
fn last_bytes(buf: &[u8], n: usize) -> &[u8] {
    &buf[buf.len().saturating_sub(n)..]
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;
    use std::io::Write;

    /// Wrapper so we can drive `TailArgs` through clap parsing in tests.
    #[derive(Parser)]
    struct TailCli {
        #[command(flatten)]
        args: TailArgs,
    }

    #[test]
    fn omitted_file_means_stdin() {
        let cli = TailCli::try_parse_from(["tail"]).unwrap();
        assert_eq!(cli.args.file, None);
        let cli = TailCli::try_parse_from(["tail", "-"]).unwrap();
        assert_eq!(cli.args.file, Some(PathBuf::from("-")));
    }

    #[test]
    fn dash_n_spelling_parses_and_conflicts_with_positional() {
        let cli = TailCli::try_parse_from(["tail", "-n", "50", "f.txt"]).unwrap();
        assert_eq!(cli.args.num_lines, Some(50));
        assert_eq!(cli.args.file, Some(PathBuf::from("f.txt")));
        let cli = TailCli::try_parse_from(["tail", "--lines", "50"]).unwrap();
        assert_eq!(cli.args.num_lines, Some(50));
        assert_eq!(cli.args.file, None);
        // Positional N and -n disagree about the count: a clap error, not a pick.
        assert!(TailCli::try_parse_from(["tail", "-n", "50", "f.txt", "20"]).is_err());
        assert!(TailCli::try_parse_from(["tail", "--bytes", "4", "-n", "50"]).is_err());
    }

    #[test]
    fn for_requires_follow() {
        assert!(TailCli::try_parse_from(["tail", "--for", "5s", "f.txt"]).is_err());
        assert!(TailCli::try_parse_from(["tail", "--follow", "--for", "5s", "f.txt"]).is_ok());
    }

    #[test]
    fn follow_window_defaults_and_zero_means_unbounded() {
        // Regression (sak-llm-zmok): --follow alone used to poll forever.
        assert_eq!(follow_window(None).unwrap(), Some(Duration::from_secs(30)));
        assert_eq!(follow_window(Some("0")).unwrap(), None);
        assert_eq!(
            follow_window(Some("1m30s")).unwrap(),
            Some(Duration::from_secs(90))
        );
        assert!(follow_window(Some("soon")).is_err());
    }

    #[test]
    fn follow_lines_continues_numbering_and_stops_at_window() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("app.log");
        std::fs::write(&p, "a\nb\n").unwrap();
        let writer_path = p.clone();
        let appender = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(100));
            let mut f = std::fs::OpenOptions::new()
                .append(true)
                .open(writer_path)
                .unwrap();
            f.write_all(b"c\n").unwrap();
        });
        let mut out = Vec::new();
        let started = Instant::now();
        let outcome =
            follow_lines(&p, 1, false, Some(Duration::from_millis(700)), &mut out).unwrap();
        appender.join().unwrap();
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "window not honored"
        );
        assert_eq!(outcome, Outcome::Found);
        assert_eq!(String::from_utf8(out).unwrap(), "2\tb\n3\tc\n");
    }

    #[test]
    fn follow_without_growth_is_not_found() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("quiet.log");
        std::fs::write(&p, "only\n").unwrap();
        let mut out = Vec::new();
        let window = Some(Duration::from_millis(50));
        assert_eq!(
            follow_lines(&p, 10, true, window, &mut out).unwrap(),
            Outcome::NotFound
        );
        assert_eq!(out, b"only\n");
        let mut out = Vec::new();
        assert_eq!(
            follow_bytes(&p, 2, window, &mut out).unwrap(),
            Outcome::NotFound
        );
    }

    #[test]
    fn follow_lines_tolerates_invalid_utf8() {
        // The initial read used to be read_to_string, which errored here.
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("bin.log");
        std::fs::write(&p, b"ok\n\xff\xfe\n").unwrap();
        let mut out = Vec::new();
        follow_lines(&p, 10, true, Some(Duration::from_millis(10)), &mut out).unwrap();
        assert_eq!(String::from_utf8(out).unwrap(), "ok\n\u{fffd}\u{fffd}\n");
    }

    #[test]
    fn follow_initial_numbers_last_n() {
        let (initial, pending, next) = follow_initial("a\nb\nc\nd\ne\n", 2);
        assert_eq!(initial, vec![(4, "d".to_string()), (5, "e".to_string())]);
        assert!(pending.is_empty());
        assert_eq!(next, 6);
    }

    #[test]
    fn follow_initial_buffers_partial_tail() {
        // No trailing newline: the partial line was printed as 3 and its
        // completion reuses 3 rather than advancing to 4.
        let (initial, pending, next) = follow_initial("a\nb\nc", 10);
        assert_eq!(initial.len(), 3);
        assert_eq!(pending, b"c");
        assert_eq!(next, 3);
    }

    #[test]
    fn follow_initial_empty_content() {
        let (initial, pending, next) = follow_initial("", 10);
        assert!(initial.is_empty());
        assert!(pending.is_empty());
        assert_eq!(next, 1);
    }

    #[test]
    fn take_complete_lines_buffers_partial() {
        let mut buf = b"one\ntwo\npar".to_vec();
        assert_eq!(take_complete_lines(&mut buf), ["one", "two"]);
        assert_eq!(buf, b"par");
        // Completion arrives later: the buffered prefix joins it.
        buf.extend_from_slice(b"tial\nnext\n");
        assert_eq!(take_complete_lines(&mut buf), ["partial", "next"]);
        assert!(buf.is_empty());
    }

    fn write_lines(path: &std::path::Path, n: usize) {
        let mut f = std::fs::File::create(path).unwrap();
        for i in 1..=n {
            writeln!(f, "line {i}").unwrap();
        }
    }

    #[test]
    fn tail_lines_default() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("f.txt");
        write_lines(&p, 50);
        let args = TailArgs {
            file: Some(p),
            lines: None,
            num_lines: None,
            bytes: None,
            no_line_numbers: true,
            follow: false,
            follow_for: None,
        };
        assert_eq!(run(&args).unwrap(), Outcome::Found);
    }

    #[test]
    fn tail_bytes_mode() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("f.bin");
        std::fs::write(&p, b"abcdefghij").unwrap();
        let args = TailArgs {
            file: Some(p),
            lines: None,
            num_lines: None,
            bytes: Some(3),
            follow: false,
            follow_for: None,
            no_line_numbers: false,
        };
        assert_eq!(run(&args).unwrap(), Outcome::Found);
    }

    #[test]
    fn tail_bytes_clamps_to_file_len() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("f.bin");
        std::fs::write(&p, b"abc").unwrap();
        let args = TailArgs {
            file: Some(p),
            lines: None,
            num_lines: None,
            bytes: Some(1000),
            follow: false,
            follow_for: None,
            no_line_numbers: false,
        };
        assert_eq!(run(&args).unwrap(), Outcome::Found);
    }

    #[test]
    fn last_bytes_takes_tail_and_clamps() {
        // The stdin byte path slices the last n bytes, clamping when n > len.
        assert_eq!(last_bytes(b"abcdefghij", 3), b"hij");
        assert_eq!(last_bytes(b"abc", 10), b"abc");
        assert_eq!(last_bytes(b"", 5), b"");
    }

    #[test]
    fn tail_empty_file_exit_1() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("empty.txt");
        std::fs::write(&p, b"").unwrap();
        let args = TailArgs {
            file: Some(p),
            lines: None,
            num_lines: None,
            bytes: Some(10),
            follow: false,
            follow_for: None,
            no_line_numbers: false,
        };
        assert_eq!(run(&args).unwrap(), Outcome::NotFound);
    }
}
