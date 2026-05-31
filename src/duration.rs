//! Shared compact-duration parsing, used across domains.
//!
//! Accepts a compact compound form: a sequence of `<digits><unit>` segments
//! where unit is one of `s`, `m`, `h`, `d`, `w` (e.g. `30s`, `5m`, `2h30m`,
//! `1d12h`, `1w`). Returns the total as a number of seconds.
//!
//! We roll our own rather than pulling in `humantime` — the parser is small
//! enough to test exhaustively, and the domains that lean on it (`prom`,
//! `docker`) deliberately keep their dependency surface minimal. This started
//! life in `src/prom/duration.rs` and was promoted to the crate root so the
//! `docker` domain (`sak docker logs --since`) can reuse it without depending
//! on the `prom` cargo feature.

use anyhow::{Result, anyhow};

/// Parse a compound duration string into a total number of seconds.
///
/// Units: `s` (seconds), `m` (minutes), `h` (hours), `d` (days),
/// `w` (weeks). Segments may be chained (`1h30m`); whitespace is rejected.
pub fn parse_duration(s: &str) -> Result<u64> {
    if s.is_empty() {
        return Err(anyhow!("empty duration"));
    }
    let mut total: u64 = 0;
    let mut num: u64 = 0;
    let mut had_digit = false;
    let mut had_unit = false;
    for ch in s.chars() {
        if let Some(d) = ch.to_digit(10) {
            num = num
                .checked_mul(10)
                .and_then(|n| n.checked_add(d as u64))
                .ok_or_else(|| anyhow!("duration overflow: {s}"))?;
            had_digit = true;
        } else {
            if !had_digit {
                return Err(anyhow!("invalid duration `{s}`: unit without digits"));
            }
            let mult: u64 = match ch {
                's' => 1,
                'm' => 60,
                'h' => 3_600,
                'd' => 86_400,
                'w' => 604_800,
                _ => {
                    return Err(anyhow!(
                        "invalid duration unit `{ch}` in `{s}` (expected s/m/h/d/w)"
                    ));
                }
            };
            let segment = num
                .checked_mul(mult)
                .ok_or_else(|| anyhow!("duration overflow: {s}"))?;
            total = total
                .checked_add(segment)
                .ok_or_else(|| anyhow!("duration overflow: {s}"))?;
            num = 0;
            had_digit = false;
            had_unit = true;
        }
    }
    if had_digit {
        return Err(anyhow!(
            "invalid duration `{s}`: trailing digits without unit"
        ));
    }
    if !had_unit {
        return Err(anyhow!("invalid duration `{s}`"));
    }
    Ok(total)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn simple_units() {
        assert_eq!(parse_duration("10s").unwrap(), 10);
        assert_eq!(parse_duration("5m").unwrap(), 300);
        assert_eq!(parse_duration("2h").unwrap(), 7_200);
        assert_eq!(parse_duration("1d").unwrap(), 86_400);
        assert_eq!(parse_duration("1w").unwrap(), 604_800);
    }

    #[test]
    fn compound() {
        assert_eq!(parse_duration("2h30m").unwrap(), 2 * 3_600 + 30 * 60);
        assert_eq!(parse_duration("1d12h").unwrap(), 86_400 + 12 * 3_600);
        assert_eq!(parse_duration("1h30m45s").unwrap(), 3_600 + 30 * 60 + 45);
        assert_eq!(parse_duration("1w1d").unwrap(), 604_800 + 86_400);
    }

    #[test]
    fn rejects_invalid() {
        assert!(parse_duration("").is_err()); // empty
        assert!(parse_duration("10").is_err()); // no unit
        assert!(parse_duration("m").is_err()); // unit without digits
        assert!(parse_duration("10x").is_err()); // unknown unit
        assert!(parse_duration("10s5").is_err()); // trailing digits
        assert!(parse_duration("1 0s").is_err()); // whitespace
        assert!(parse_duration("1y").is_err()); // y not supported
    }

    #[test]
    fn overflow_is_an_error() {
        // u64::MAX seconds * the weeks multiplier is guaranteed to overflow.
        assert!(parse_duration("99999999999999999999w").is_err());
    }
}
