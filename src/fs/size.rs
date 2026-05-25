//! Byte-size parsing and human-readable formatting shared by the disk-triage
//! commands (`largest`, `duplicates`, `find`). Sizes use binary (1024-based)
//! units throughout — the suffix is a single letter K/M/G/T/P (optionally
//! followed by a bare `B`), so `1M` is 1048576 bytes, matching `du`/`ls -h`.

use anyhow::{Result, bail};

/// Parse a byte size like `1024`, `4K`, `1.5M`, `2G` into a byte count.
///
/// Accepts an optional binary suffix (K/M/G/T/P, case-insensitive), with an
/// optional trailing `B` (`1KB` == `1K`). A bare number is bytes. Fractional
/// values are allowed for suffixed sizes (`1.5M`) but not for bare byte counts.
pub fn parse_size(s: &str) -> Result<u64> {
    let s = s.trim();
    if s.is_empty() {
        bail!("empty size");
    }
    // Split the numeric prefix from the unit suffix.
    let split = s
        .find(|c: char| !(c.is_ascii_digit() || c == '.'))
        .unwrap_or(s.len());
    let (num, unit) = s.split_at(split);
    if num.is_empty() {
        bail!("size missing a number: {s}");
    }
    let unit = unit.trim();
    // Allow a trailing 'B' on any suffix (KB, MB, ...) and on bare bytes (12B).
    let unit = unit.strip_suffix(['B', 'b']).unwrap_or(unit);
    let mult: u64 = match unit {
        "" => 1,
        u if u.eq_ignore_ascii_case("k") => 1024,
        u if u.eq_ignore_ascii_case("m") => 1024u64.pow(2),
        u if u.eq_ignore_ascii_case("g") => 1024u64.pow(3),
        u if u.eq_ignore_ascii_case("t") => 1024u64.pow(4),
        u if u.eq_ignore_ascii_case("p") => 1024u64.pow(5),
        other => bail!("unknown size unit: {other:?} (use K, M, G, T, P)"),
    };
    let value: f64 = num
        .parse()
        .map_err(|_| anyhow::anyhow!("invalid size number: {num:?}"))?;
    if value < 0.0 {
        bail!("size cannot be negative: {s}");
    }
    Ok((value * mult as f64) as u64)
}

/// Format a byte count as a short human-readable string: bytes get a plain `B`
/// suffix (`512B`), larger sizes use one decimal place with a binary unit
/// (`1.2K`, `4.5M`, `1.1G`).
pub fn human(bytes: u64) -> String {
    const UNITS: [&str; 6] = ["B", "K", "M", "G", "T", "P"];
    let mut size = bytes as f64;
    let mut unit = 0;
    while size >= 1024.0 && unit < UNITS.len() - 1 {
        size /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes}B")
    } else {
        format!("{size:.1}{}", UNITS[unit])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_plain_bytes() {
        assert_eq!(parse_size("0").unwrap(), 0);
        assert_eq!(parse_size("1024").unwrap(), 1024);
        assert_eq!(parse_size("12B").unwrap(), 12);
    }

    #[test]
    fn parse_suffixed() {
        assert_eq!(parse_size("1K").unwrap(), 1024);
        assert_eq!(parse_size("1k").unwrap(), 1024);
        assert_eq!(parse_size("1KB").unwrap(), 1024);
        assert_eq!(parse_size("1M").unwrap(), 1024 * 1024);
        assert_eq!(parse_size("2G").unwrap(), 2 * 1024u64.pow(3));
        assert_eq!(parse_size("1.5M").unwrap(), 1024 * 1024 * 3 / 2);
    }

    #[test]
    fn parse_rejects_garbage() {
        assert!(parse_size("").is_err());
        assert!(parse_size("M").is_err());
        assert!(parse_size("1X").is_err());
        assert!(parse_size("-1K").is_err());
        assert!(parse_size("abc").is_err());
    }

    #[test]
    fn human_readable() {
        assert_eq!(human(0), "0B");
        assert_eq!(human(512), "512B");
        assert_eq!(human(1024), "1.0K");
        assert_eq!(human(1234), "1.2K");
        assert_eq!(human(4_718_592), "4.5M");
        assert_eq!(human(1_181_116_006), "1.1G");
    }
}
