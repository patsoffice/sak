use crate::output::Outcome;
use std::io::{self, Write};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::PathBuf;
use std::time::UNIX_EPOCH;

use anyhow::Result;
use clap::{Args, ValueEnum};
use serde::Serialize;

use super::size::human;
use crate::output::BoundedWriter;

#[derive(Clone, ValueEnum)]
pub enum Format {
    /// Tab-separated row per path with human-readable sizes
    Human,
    /// JSON array with raw byte counts
    Json,
}

#[derive(Args)]
#[command(
    about = "Show file/directory metadata (size, perms, mtime, type)",
    long_about = "Show metadata for one or more paths.\n\n\
        For every PATH, reports its type (file/dir/symlink), octal permission \
        bits, size, modification time (UTC, ISO 8601), and — for directories — \
        the number of entries it contains. The entry's own type is reported \
        (a symlink shows as `symlink`, not its target), so this never follows \
        links.\n\n\
        --format human (the default) prints one tab-separated row per path \
        (`type<TAB>perms<TAB>size<TAB>mtime<TAB>entries<TAB>path`) with \
        human-readable sizes; --format json prints a JSON array with raw byte \
        counts and unix-seconds timestamps. --printf FMT prints one custom \
        line per path in a GNU stat subset (%n name, %s size, %b blocks, %B \
        block size, %u/%g numeric uid/gid, %a octal perms, %A symbolic perms, \
        %F file type, %i inode, %h link count, %d device, %x/%y/%z ISO-8601 \
        atime/mtime/ctime, %X/%Y/%Z epoch atime/mtime/ctime, %% literal \
        percent; no trailing newline is added). Backslash escapes (\n, \t, \
        \\ and friends) work as in GNU. Times are UTC, unlike GNU's \
        local time; user/group NAME lookup (%U/%G) is unsupported. --printf \
        bypasses --limit. A path that can't be stat'd is \
        reported to stderr and makes the exit code 2.",
    after_help = "\
Examples:
  sak fs stat src/main.rs                  Metadata for one file
  sak fs stat src/ Cargo.toml README.md    Several paths at once
  sak fs stat --format json src/           JSON with raw bytes
  sak fs stat --printf '%n %s bytes\n' a b   Custom format per path"
)]
pub struct StatArgs {
    /// Paths to stat
    #[arg(required = true)]
    pub paths: Vec<PathBuf>,

    /// Output format
    #[arg(long, value_enum, default_value = "human")]
    pub format: Format,

    /// Maximum number of lines to output
    #[arg(long)]
    pub limit: Option<usize>,

    /// Custom per-path output in a GNU stat subset (see above)
    #[arg(long, value_name = "FORMAT")]
    pub printf: Option<String>,
}

#[derive(Serialize)]
struct StatRecord {
    path: String,
    #[serde(rename = "type")]
    kind: &'static str,
    size_bytes: u64,
    mode: String,
    mtime_unix: i64,
    mtime_iso: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    entries: Option<usize>,
}

/// Collect metadata for `path`, or `Err` with a human message if it can't be
/// stat'd. Uses `symlink_metadata` so a symlink reports as itself, not its
/// target.
fn stat_one(path: &PathBuf) -> Result<StatRecord> {
    let meta = std::fs::symlink_metadata(path)
        .map_err(|e| anyhow::anyhow!("cannot stat {}: {e}", path.display()))?;
    let ft = meta.file_type();
    let kind = if ft.is_symlink() {
        "symlink"
    } else if ft.is_dir() {
        "dir"
    } else {
        "file"
    };
    let mode = meta.permissions().mode() & 0o7777;
    let mtime_unix = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    // Directory entry count — only meaningful for real directories.
    let entries = if ft.is_dir() {
        std::fs::read_dir(path).ok().map(|rd| rd.count())
    } else {
        None
    };
    Ok(StatRecord {
        path: path.display().to_string(),
        kind,
        size_bytes: meta.len(),
        mode: format!("{mode:04o}"),
        mtime_unix,
        mtime_iso: iso8601_utc(mtime_unix),
        entries,
    })
}

/// Format unix seconds as `YYYY-MM-DDTHH:MM:SSZ` (UTC). Pure integer math via
/// the civil-from-days algorithm, so no chrono dependency is needed.
fn iso8601_utc(secs: i64) -> String {
    let days = secs.div_euclid(86_400);
    let tod = secs.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    let (h, min, s) = (tod / 3600, (tod % 3600) / 60, tod % 60);
    format!("{y:04}-{m:02}-{d:02}T{h:02}:{min:02}:{s:02}Z")
}

/// Directives `--printf` understands. A strict subset of GNU stat: no
/// user/group NAME lookup (`%U`/`%G` would need a new dependency), no mount
/// point (`%m`), no birth time (`%w` — not portable).
const PRINTF_DIRECTIVES: &str = "nsbBuagAFihdxyzXYZ%";

/// One compiled `--printf` piece: literal text or a single directive.
enum PrintfItem {
    Lit(String),
    Dir(char),
}

/// Compile a `--printf` format. Unknown directives and a dangling `%` are
/// tool errors (exit 2) naming the supported set, so a typo can't silently
/// print garbage.
fn parse_printf(fmt: &str) -> Result<Vec<PrintfItem>> {
    let mut items = Vec::new();
    let mut lit = String::new();
    let mut chars = fmt.chars();
    while let Some(c) = chars.next() {
        if c == '%' {
            match chars.next() {
                None => {
                    anyhow::bail!("dangling % in --printf format (supported: %{PRINTF_DIRECTIVES})")
                }
                Some('%') => lit.push('%'),
                Some(d) if PRINTF_DIRECTIVES.contains(d) => {
                    if !lit.is_empty() {
                        items.push(PrintfItem::Lit(std::mem::take(&mut lit)));
                    }
                    items.push(PrintfItem::Dir(d));
                }
                Some(d) => {
                    anyhow::bail!(
                        "unsupported stat directive %{d} (supported: %{PRINTF_DIRECTIVES})"
                    )
                }
            }
            continue;
        }
        // GNU-style backslash escapes in literals; an unknown escape keeps
        // both characters so nothing is silently swallowed.
        if c == '\\' {
            match chars.next() {
                None => lit.push('\\'),
                Some('a') => lit.push('\x07'),
                Some('b') => lit.push('\x08'),
                Some('f') => lit.push('\x0C'),
                Some('n') => lit.push('\n'),
                Some('r') => lit.push('\r'),
                Some('t') => lit.push('\t'),
                Some('v') => lit.push('\x0B'),
                Some('\\') => lit.push('\\'),
                Some(other) => {
                    lit.push('\\');
                    lit.push(other);
                }
            }
            continue;
        }
        lit.push(c);
    }
    if !lit.is_empty() {
        items.push(PrintfItem::Lit(lit));
    }
    Ok(items)
}

/// Render one path through a compiled `--printf` template. Uses
/// `symlink_metadata` like [`stat_one`], so a symlink reports as itself.
fn render_printf(tmpl: &[PrintfItem], path: &PathBuf) -> Result<String> {
    let meta = std::fs::symlink_metadata(path)
        .map_err(|e| anyhow::anyhow!("cannot stat {}: {e}", path.display()))?;
    let ft = meta.file_type();
    let mode = meta.permissions().mode();
    let epoch = |t: std::io::Result<std::time::SystemTime>| {
        t.ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0)
    };
    let mut out = String::new();
    for item in tmpl {
        match item {
            PrintfItem::Lit(s) => out.push_str(s),
            PrintfItem::Dir(d) => out.push_str(&match d {
                'n' => path.display().to_string(),
                's' => meta.len().to_string(),
                'b' => meta.blocks().to_string(),
                'B' => meta.blksize().to_string(),
                'u' => meta.uid().to_string(),
                'g' => meta.gid().to_string(),
                'a' => format!("{:o}", mode & 0o7777),
                'A' => symbolic_perms(ft, mode),
                'F' => file_type_name(ft).to_string(),
                'i' => meta.ino().to_string(),
                'h' => meta.nlink().to_string(),
                'd' => meta.dev().to_string(),
                'x' => iso8601_utc(epoch(meta.accessed())),
                'y' => iso8601_utc(epoch(meta.modified())),
                'z' => iso8601_utc(meta.ctime()),
                'X' => epoch(meta.accessed()).to_string(),
                'Y' => epoch(meta.modified()).to_string(),
                'Z' => meta.ctime().to_string(),
                // parse_printf only emits supported directives.
                _ => unreachable!("unsupported directive %{d} passed validation"),
            }),
        }
    }
    Ok(out)
}

/// `drwxr-xr-x`-style permissions: filetype char plus rwx triplets.
fn symbolic_perms(ft: std::fs::FileType, mode: u32) -> String {
    use std::os::unix::fs::FileTypeExt;

    let mut s = String::with_capacity(10);
    s.push(if ft.is_dir() {
        'd'
    } else if ft.is_symlink() {
        'l'
    } else if ft.is_block_device() {
        'b'
    } else if ft.is_char_device() {
        'c'
    } else if ft.is_fifo() {
        'p'
    } else if ft.is_socket() {
        's'
    } else {
        '-'
    });
    for shift in [6, 3, 0] {
        let bits = (mode >> shift) & 0o7;
        s.push(if bits & 0o4 != 0 { 'r' } else { '-' });
        s.push(if bits & 0o2 != 0 { 'w' } else { '-' });
        s.push(if bits & 0o1 != 0 { 'x' } else { '-' });
    }
    s
}

/// GNU-style file type name for `%F`.
fn file_type_name(ft: std::fs::FileType) -> &'static str {
    use std::os::unix::fs::FileTypeExt;

    if ft.is_dir() {
        "directory"
    } else if ft.is_symlink() {
        "symbolic link"
    } else if ft.is_block_device() {
        "block special file"
    } else if ft.is_char_device() {
        "character special file"
    } else if ft.is_fifo() {
        "fifo"
    } else if ft.is_socket() {
        "socket"
    } else {
        "regular file"
    }
}

/// Inverse of `days_from_civil` (Howard Hinnant): days since 1970-01-01 to a
/// proleptic-Gregorian (year, month, day).
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32; // [1, 12]
    (if m <= 2 { y + 1 } else { y }, m, d)
}

pub fn run(args: &StatArgs) -> Result<Outcome> {
    // --printf bypasses the record table (and --limit): the format string
    // owns newlines, so line-counting output would corrupt custom formats.
    if let Some(fmt) = &args.printf {
        let tmpl = parse_printf(fmt)?;
        let stdout = io::stdout();
        let mut out = stdout.lock();
        let mut any_error = false;
        for path in &args.paths {
            match render_printf(&tmpl, path) {
                Ok(line) => {
                    out.write_all(line.as_bytes())?;
                }
                Err(e) => {
                    eprintln!("sak: error: {e}");
                    any_error = true;
                }
            }
        }
        out.flush()?;
        return Ok(if any_error {
            Outcome::Partial
        } else {
            Outcome::Found
        });
    }

    let mut records: Vec<StatRecord> = Vec::new();
    let mut any_error = false;
    for path in &args.paths {
        match stat_one(path) {
            Ok(rec) => records.push(rec),
            Err(e) => {
                eprintln!("sak: error: {e}");
                any_error = true;
            }
        }
    }

    let stdout = io::stdout();
    let mut writer = BoundedWriter::new(stdout.lock(), args.limit);

    match args.format {
        Format::Json => {
            let pretty = serde_json::to_string_pretty(&records)?;
            for line in pretty.lines() {
                if !writer.write_line(line)? {
                    break;
                }
            }
        }
        Format::Human => {
            for rec in &records {
                let size = human(rec.size_bytes);
                let entries = rec
                    .entries
                    .map(|n| n.to_string())
                    .unwrap_or_else(|| "-".to_string());
                let row = format!(
                    "{}\t{}\t{}\t{}\t{}\t{}",
                    rec.kind, rec.mode, size, rec.mtime_iso, entries, rec.path
                );
                if !writer.write_line(&row)? {
                    break;
                }
            }
        }
    }
    writer.flush()?;

    if any_error {
        Ok(Outcome::Partial)
    } else {
        Ok(Outcome::Found)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn civil_roundtrips_known_epochs() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(946_684_800 / 86_400), (2000, 1, 1));
        assert_eq!(civil_from_days(1_767_225_600 / 86_400), (2026, 1, 1));
    }

    #[test]
    fn iso_formats_with_time_of_day() {
        // 2000-01-01T00:00:00Z
        assert_eq!(iso8601_utc(946_684_800), "2000-01-01T00:00:00Z");
        // One hour, one minute, one second past that epoch.
        assert_eq!(iso8601_utc(946_684_800 + 3661), "2000-01-01T01:01:01Z");
    }

    #[test]
    fn stat_file_and_dir() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "hello").unwrap();
        std::fs::write(dir.path().join("b.txt"), "world").unwrap();

        let file = stat_one(&dir.path().join("a.txt")).unwrap();
        assert_eq!(file.kind, "file");
        assert_eq!(file.size_bytes, 5);
        assert_eq!(file.entries, None);

        let d = stat_one(&dir.path().to_path_buf()).unwrap();
        assert_eq!(d.kind, "dir");
        assert_eq!(d.entries, Some(2));
    }

    #[test]
    fn missing_path_is_exit_2() {
        let args = StatArgs {
            paths: vec![PathBuf::from("/no/such/path/xyz")],
            format: Format::Human,
            limit: None,
            printf: None,
        };
        assert_eq!(run(&args).unwrap(), Outcome::Partial);
    }

    #[test]
    fn existing_path_is_exit_0() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "hi").unwrap();
        let args = StatArgs {
            paths: vec![dir.path().join("a.txt")],
            format: Format::Json,
            limit: None,
            printf: None,
        };
        assert_eq!(run(&args).unwrap(), Outcome::Found);
    }

    #[test]
    fn printf_unknown_directive_is_tool_error() {
        assert!(parse_printf("%U").is_err());
        assert!(parse_printf("size=%s trailing=%").is_err());
    }

    #[test]
    fn printf_interprets_backslash_escapes() {
        let tmpl = parse_printf("%s\\n\\tindented\\\\done").unwrap();
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.txt");
        std::fs::write(&p, "hello").unwrap();
        assert_eq!(render_printf(&tmpl, &p).unwrap(), "5\n\tindented\\done");
    }

    #[test]
    fn printf_renders_name_size_type() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.txt");
        std::fs::write(&p, "hello").unwrap();
        let tmpl = parse_printf("%n|%s|%F|%%").unwrap();
        let line = render_printf(&tmpl, &p).unwrap();
        assert_eq!(line, format!("{}|5|regular file|%", p.display()));
    }

    #[test]
    fn printf_renders_dir_and_symbolic_perms() {
        let dir = tempfile::tempdir().unwrap();
        let tmpl = parse_printf("%F|%A|%a").unwrap();
        let line = render_printf(&tmpl, &dir.path().to_path_buf()).unwrap();
        let mut parts = line.split('|');
        assert_eq!(parts.next(), Some("directory"));
        // Symbolic form is always 10 chars starting with 'd'; octal perms
        // depend on umask, so only assert the shape, not the value.
        let sym = parts.next().unwrap();
        assert_eq!(sym.len(), 10);
        assert!(sym.starts_with('d'));
        let oct = parts.next().unwrap();
        assert!(!oct.is_empty() && oct.len() <= 4);
        assert!(oct.chars().all(|c| ('0'..='7').contains(&c)));
    }

    #[test]
    fn printf_missing_path_is_exit_2() {
        let args = StatArgs {
            paths: vec![PathBuf::from("/no/such/path/xyz")],
            format: Format::Human,
            limit: None,
            printf: Some("%n".to_string()),
        };
        assert_eq!(run(&args).unwrap(), Outcome::Partial);
    }

    #[test]
    fn printf_bad_format_is_exit_2() {
        let args = StatArgs {
            paths: vec![PathBuf::from("x")],
            format: Format::Human,
            limit: None,
            printf: Some("%U".to_string()),
        };
        assert!(run(&args).is_err());
    }
}
