use crate::output::Outcome;
use std::io;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, anyhow, bail};
use clap::{Args, ValueEnum};
use globset::{Glob, GlobMatcher};

use super::size::parse_size;
use super::{is_hidden_file, pruned_walk};
use crate::output::{BoundedWriter, relative_path};

#[derive(Clone, ValueEnum)]
pub enum FindType {
    /// Regular file
    F,
    /// Directory
    D,
    /// Symbolic link
    L,
}

#[derive(Args)]
#[command(
    about = "Find files by metadata (size, mtime, type, name)",
    long_about = "Find files by metadata predicates.\n\n\
        Where `glob` matches by name only, `find` filters the walk by size, \
        modification time, entry type, and (optionally) a name glob. Predicates \
        combine with AND — an entry must satisfy every one given. Matching \
        paths are printed one per line, sorted alphabetically.\n\n\
        --size takes `+N` (larger than), `-N` (smaller than), `A-B` (inclusive \
        range), or `N` (exact); N accepts K/M/G/T/P binary suffixes. --mtime \
        takes `+D` (older than), `-D` (newer than) where D is a duration like \
        `7d`/`1h`/`30m`, or `YYYY-MM-DD..YYYY-MM-DD` for a date range (start \
        inclusive, end exclusive, UTC). The same directory pruning as `glob` \
        applies (.git, target, node_modules, ...; dotfiles unless --hidden).",
    after_help = "\
Examples:
  sak fs find . --size +1M                 Files larger than 1 MiB
  sak fs find src --size 4K-1M             Files between 4 KiB and 1 MiB
  sak fs find . --mtime -1h                Modified in the last hour
  sak fs find . --mtime +7d --type f       Files untouched for over a week
  sak fs find . --type d --name 'test*'    Directories named test*
  sak fs find . --mtime 2026-01-01..2026-02-01   Modified in January 2026"
)]
pub struct FindArgs {
    /// Directory to search
    pub path: PathBuf,

    /// Size predicate: +1M (larger), -10K (smaller), 4K-1M (range), 4K (exact).
    /// allow_hyphen_values lets a leading-`-` value (e.g. -10K) parse as the
    /// flag's argument rather than being mistaken for another option.
    #[arg(long, value_name = "EXPR", allow_hyphen_values = true)]
    pub size: Option<String>,

    /// Mtime predicate: +7d (older), -1h (newer), or YYYY-MM-DD..YYYY-MM-DD
    #[arg(long, value_name = "EXPR", allow_hyphen_values = true)]
    pub mtime: Option<String>,

    /// Entry type: f (file), d (directory), l (symlink)
    #[arg(short = 't', long = "type")]
    pub entry_type: Option<FindType>,

    /// Restrict to entries whose name matches this glob (same syntax as `glob`)
    #[arg(long, value_name = "GLOB")]
    pub name: Option<String>,

    /// Include hidden files and directories (dotfiles)
    #[arg(short = 'H', long)]
    pub hidden: bool,

    /// Follow symbolic links
    #[arg(short = 'L', long)]
    pub follow_links: bool,

    /// Maximum number of results to return
    #[arg(long)]
    pub limit: Option<usize>,
}

/// A parsed `--size` predicate over a byte count.
#[derive(Debug, PartialEq)]
enum SizePred {
    /// `+N` — strictly larger than N
    Min(u64),
    /// `-N` — strictly smaller than N
    Max(u64),
    /// `A-B` — inclusive range [A, B]
    Range(u64, u64),
    /// `N` — exactly N bytes
    Exact(u64),
}

impl SizePred {
    fn parse(s: &str) -> Result<Self> {
        let s = s.trim();
        if let Some(rest) = s.strip_prefix('+') {
            return Ok(SizePred::Min(parse_size(rest)?));
        }
        if let Some(rest) = s.strip_prefix('-') {
            return Ok(SizePred::Max(parse_size(rest)?));
        }
        // Range `A-B`: split on the first '-' that isn't the leading sign.
        if let Some(idx) = s.find('-') {
            let (lo, hi) = s.split_at(idx);
            let hi = &hi[1..];
            let lo = parse_size(lo)?;
            let hi = parse_size(hi)?;
            if lo > hi {
                bail!("size range low bound exceeds high bound: {s}");
            }
            return Ok(SizePred::Range(lo, hi));
        }
        Ok(SizePred::Exact(parse_size(s)?))
    }

    fn matches(&self, len: u64) -> bool {
        match *self {
            SizePred::Min(n) => len > n,
            SizePred::Max(n) => len < n,
            SizePred::Range(lo, hi) => len >= lo && len <= hi,
            SizePred::Exact(n) => len == n,
        }
    }
}

/// A parsed `--mtime` predicate over a unix-seconds modification time.
#[derive(Debug, PartialEq)]
enum TimePred {
    /// `+D` — modified more than D seconds ago (mtime < now - D)
    OlderThan(i64),
    /// `-D` — modified within the last D seconds (mtime > now - D)
    NewerThan(i64),
    /// `A..B` — mtime in [A, B) (start inclusive, end exclusive), unix seconds
    Range(i64, i64),
}

impl TimePred {
    fn parse(s: &str, now: i64) -> Result<Self> {
        let s = s.trim();
        if let Some((a, b)) = s.split_once("..") {
            let lo = parse_date(a.trim())?;
            let hi = parse_date(b.trim())?;
            if lo > hi {
                bail!("mtime range start is after end: {s}");
            }
            return Ok(TimePred::Range(lo, hi));
        }
        if let Some(rest) = s.strip_prefix('+') {
            return Ok(TimePred::OlderThan(now - parse_duration(rest)?));
        }
        if let Some(rest) = s.strip_prefix('-') {
            return Ok(TimePred::NewerThan(now - parse_duration(rest)?));
        }
        bail!("mtime expects +DUR, -DUR, or DATE..DATE (got {s:?})")
    }

    fn matches(&self, mtime: i64) -> bool {
        match *self {
            TimePred::OlderThan(cut) => mtime < cut,
            TimePred::NewerThan(cut) => mtime > cut,
            TimePred::Range(lo, hi) => mtime >= lo && mtime < hi,
        }
    }
}

/// Parse a duration like `7d`, `1h`, `30m`, `45s`, `2w` into seconds.
fn parse_duration(s: &str) -> Result<i64> {
    let s = s.trim();
    let split = s.find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len());
    let (num, unit) = s.split_at(split);
    if num.is_empty() {
        bail!("duration missing a number: {s:?}");
    }
    let n: i64 = num
        .parse()
        .map_err(|_| anyhow!("invalid duration number: {num:?}"))?;
    let mult = match unit.trim() {
        "s" => 1,
        "m" => 60,
        "h" => 3600,
        "d" => 86400,
        "w" => 604800,
        other => bail!("unknown duration unit: {other:?} (use s, m, h, d, w)"),
    };
    Ok(n * mult)
}

/// Parse a `YYYY-MM-DD` date into unix seconds at UTC midnight.
fn parse_date(s: &str) -> Result<i64> {
    let parts: Vec<&str> = s.split('-').collect();
    if parts.len() != 3 {
        bail!("expected YYYY-MM-DD date: {s:?}");
    }
    let y: i64 = parts[0].parse().map_err(|_| anyhow!("bad year in {s:?}"))?;
    let m: u32 = parts[1]
        .parse()
        .map_err(|_| anyhow!("bad month in {s:?}"))?;
    let d: u32 = parts[2].parse().map_err(|_| anyhow!("bad day in {s:?}"))?;
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        bail!("date out of range: {s:?}");
    }
    Ok(days_from_civil(y, m, d) * 86400)
}

/// Days since 1970-01-01 for a proleptic-Gregorian date (Howard Hinnant's
/// `days_from_civil`). Valid for any in-range y/m/d.
fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400; // [0, 399]
    let mp = ((m + 9) % 12) as i64; // Mar=0 ... Feb=11
    let doy = (153 * mp + 2) / 5 + d as i64 - 1; // [0, 365]
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy; // [0, 146096]
    era * 146097 + doe - 719468
}

pub fn run(args: &FindArgs) -> Result<Outcome> {
    let size_pred = match &args.size {
        Some(s) => Some(SizePred::parse(s).with_context(|| format!("invalid --size: {s}"))?),
        None => None,
    };
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let time_pred = match &args.mtime {
        Some(s) => Some(TimePred::parse(s, now).with_context(|| format!("invalid --mtime: {s}"))?),
        None => None,
    };
    let name_matcher: Option<GlobMatcher> = match &args.name {
        Some(g) => Some(
            Glob::new(g)
                .with_context(|| format!("invalid --name glob: {g}"))?
                .compile_matcher(),
        ),
        None => None,
    };

    let base = args
        .path
        .canonicalize()
        .with_context(|| format!("cannot access directory: {}", args.path.display()))?;

    let mut matches: Vec<PathBuf> = Vec::new();
    for entry in pruned_walk(&base, args.hidden, args.follow_links, None) {
        let entry = match entry {
            Ok(e) => e,
            Err(e) => {
                eprintln!("sak: error: {e}");
                continue;
            }
        };
        // The root itself isn't a useful result (its relative path is empty).
        if entry.depth() == 0 {
            continue;
        }
        if is_hidden_file(&entry, args.hidden) {
            continue;
        }

        let ft = entry.file_type();
        if let Some(t) = &args.entry_type {
            let ok = match t {
                FindType::F => ft.is_file(),
                FindType::D => ft.is_dir(),
                FindType::L => ft.is_symlink(),
            };
            if !ok {
                continue;
            }
        }

        if let Some(m) = &name_matcher {
            let name = entry.file_name().to_string_lossy();
            if !m.is_match(name.as_ref()) {
                continue;
            }
        }

        // Only stat when a size/mtime predicate needs it.
        if size_pred.is_some() || time_pred.is_some() {
            let Ok(meta) = entry.metadata() else { continue };
            if let Some(sp) = &size_pred
                && !sp.matches(meta.len())
            {
                continue;
            }
            if let Some(tp) = &time_pred {
                let mtime = meta
                    .modified()
                    .ok()
                    .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                    .map(|d| d.as_secs() as i64)
                    .unwrap_or(0);
                if !tp.matches(mtime) {
                    continue;
                }
            }
        }

        matches.push(entry.path().to_path_buf());
    }

    matches.sort();

    if matches.is_empty() {
        return Ok(Outcome::NotFound);
    }

    let stdout = io::stdout();
    let mut writer = BoundedWriter::new(stdout.lock(), args.limit);
    for path in &matches {
        let rel = relative_path(path, &base);
        if !writer.write_line(&rel)? {
            break;
        }
    }
    writer.flush()?;
    Ok(Outcome::Found)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn size_pred_parse_and_match() {
        assert_eq!(SizePred::parse("+1M").unwrap(), SizePred::Min(1024 * 1024));
        assert_eq!(SizePred::parse("-10K").unwrap(), SizePred::Max(10 * 1024));
        assert_eq!(
            SizePred::parse("4K-1M").unwrap(),
            SizePred::Range(4 * 1024, 1024 * 1024)
        );
        assert_eq!(SizePred::parse("512").unwrap(), SizePred::Exact(512));

        assert!(SizePred::Min(1000).matches(1001));
        assert!(!SizePred::Min(1000).matches(1000));
        assert!(SizePred::Max(1000).matches(999));
        assert!(!SizePred::Max(1000).matches(1000));
        assert!(SizePred::Range(10, 20).matches(10));
        assert!(SizePred::Range(10, 20).matches(20));
        assert!(!SizePred::Range(10, 20).matches(21));
        assert!(SizePred::Exact(5).matches(5));
    }

    #[test]
    fn size_pred_rejects_bad_range() {
        assert!(SizePred::parse("1M-4K").is_err());
        assert!(SizePred::parse("+bad").is_err());
    }

    #[test]
    fn duration_parsing() {
        assert_eq!(parse_duration("45s").unwrap(), 45);
        assert_eq!(parse_duration("30m").unwrap(), 1800);
        assert_eq!(parse_duration("1h").unwrap(), 3600);
        assert_eq!(parse_duration("7d").unwrap(), 604800);
        assert_eq!(parse_duration("2w").unwrap(), 1209600);
        assert!(parse_duration("5x").is_err());
        assert!(parse_duration("d").is_err());
    }

    #[test]
    fn time_pred_parse_and_match() {
        let now = 1_000_000;
        // older-than 100s → cutoff at now-100; mtime before that matches.
        let older = TimePred::parse("+100s", now).unwrap();
        assert_eq!(older, TimePred::OlderThan(now - 100));
        assert!(older.matches(now - 101));
        assert!(!older.matches(now - 99));
        // newer-than 100s → mtime after now-100 matches.
        let newer = TimePred::parse("-100s", now).unwrap();
        assert!(newer.matches(now - 50));
        assert!(!newer.matches(now - 200));
    }

    #[test]
    fn date_parsing_known_epochs() {
        assert_eq!(parse_date("1970-01-01").unwrap(), 0);
        assert_eq!(parse_date("2000-01-01").unwrap(), 946_684_800);
        assert_eq!(parse_date("2026-01-01").unwrap(), 1_767_225_600);
        assert!(parse_date("2026-13-01").is_err());
        assert!(parse_date("not-a-date").is_err());
    }

    #[test]
    fn time_pred_date_range() {
        let tp = TimePred::parse("2026-01-01..2026-02-01", 0).unwrap();
        let jan15 = parse_date("2026-01-15").unwrap();
        let feb01 = parse_date("2026-02-01").unwrap();
        assert!(tp.matches(jan15));
        // end is exclusive
        assert!(!tp.matches(feb01));
    }

    #[test]
    fn find_by_size_and_type() {
        use std::io::Write;
        let dir = tempfile::tempdir().unwrap();
        std::fs::File::create(dir.path().join("big.bin"))
            .unwrap()
            .write_all(&vec![0u8; 5000])
            .unwrap();
        std::fs::File::create(dir.path().join("small.bin"))
            .unwrap()
            .write_all(&[0u8; 10])
            .unwrap();
        std::fs::create_dir(dir.path().join("subdir")).unwrap();

        let mk = |size: Option<&str>, ty: Option<FindType>, name: Option<&str>| FindArgs {
            path: dir.path().to_path_buf(),
            size: size.map(String::from),
            mtime: None,
            entry_type: ty,
            name: name.map(String::from),
            hidden: false,
            follow_links: false,
            limit: None,
        };

        // Files over 1K: only big.bin.
        assert_eq!(run(&mk(Some("+1K"), None, None)).unwrap(), Outcome::Found);
        // A directory type filter with a name that won't match → exit 1.
        assert_eq!(
            run(&mk(None, Some(FindType::D), Some("nope*"))).unwrap(),
            Outcome::NotFound
        );
        // Directory named subdir matches.
        assert_eq!(
            run(&mk(None, Some(FindType::D), Some("subdir"))).unwrap(),
            Outcome::Found
        );
    }
}
