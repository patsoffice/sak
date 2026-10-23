//! INI parsing for the config domain.
//!
//! INI has no specification, so the dialect is spelled out here and chosen
//! for *fidelity* — sak only reads, so the parse must never alter a value:
//!
//! - `[section]` headers; section and key names keep their case.
//! - `key = value` or `key: value`; the first `=` or `:` on the line splits,
//!   so `url = https://x` and `Host: db1` both parse as expected.
//! - Comments are whole lines whose first non-blank character is `;` or `#`.
//!   A `;` or `#` later in a line is part of the value (`url = a#frag`,
//!   `password = a;b`): stripping "inline comments" silently truncates real
//!   values, the worst failure for an inspection tool.
//! - Keys before any header land in a `default` section.
//! - A line with no delimiter is a valueless key (null), distinct from
//!   `key =` (empty string). Duplicate keys keep the last value.
//! - An unterminated `[header` is an error naming the line.

use serde_json::{Map, Value};

/// Section that keys appearing before any `[header]` belong to.
pub const DEFAULT_SECTION: &str = "default";

/// Parse INI text to a two-level object: section → key → string (or null
/// for a valueless key). Errors carry the 1-based line number.
pub fn parse(text: &str) -> Result<Value, (usize, String)> {
    let mut root: Map<String, Value> = Map::new();
    let mut section = DEFAULT_SECTION.to_string();

    for (idx, raw) in text.lines().enumerate() {
        let line_no = idx + 1;
        let line = raw.trim();
        if line.is_empty() || line.starts_with(';') || line.starts_with('#') {
            continue;
        }
        if let Some(rest) = line.strip_prefix('[') {
            let Some(name) = rest.strip_suffix(']') else {
                return Err((line_no, format!("unterminated section header {line:?}")));
            };
            section = name.trim().to_string();
            root.entry(section.clone())
                .or_insert_with(|| Value::Object(Map::new()));
            continue;
        }
        let (key, value) = match line.find(['=', ':']) {
            Some(pos) => (
                line[..pos].trim(),
                Value::String(line[pos + 1..].trim().to_string()),
            ),
            None => (line, Value::Null),
        };
        if key.is_empty() {
            return Err((line_no, "key cannot be empty".to_string()));
        }
        let entry = root
            .entry(section.clone())
            .or_insert_with(|| Value::Object(Map::new()));
        if let Value::Object(map) = entry {
            map.insert(key.to_string(), value);
        }
    }
    Ok(Value::Object(root))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn sections_keys_and_both_delimiters() {
        let v = parse("[server]\nhost = a\nport: 8080\n[db]\nname=main").unwrap();
        assert_eq!(
            v,
            json!({"server": {"host": "a", "port": "8080"}, "db": {"name": "main"}})
        );
    }

    #[test]
    fn values_keep_hash_semicolon_and_later_delimiters() {
        // Regression: inline-comment stripping truncated these values.
        let v = parse("[d]\nurl = https://db.example/x#frag\npassword = a;b\nproxy: http://h:8080")
            .unwrap();
        assert_eq!(v["d"]["url"], "https://db.example/x#frag");
        assert_eq!(v["d"]["password"], "a;b");
        assert_eq!(v["d"]["proxy"], "http://h:8080");
    }

    #[test]
    fn names_keep_their_case() {
        // Regression: everything was lowercased, so `.Database.URL` missed.
        let v = parse("[Database]\nURL = x").unwrap();
        assert_eq!(v["Database"]["URL"], "x");
        assert!(v.get("database").is_none());
    }

    #[test]
    fn whole_line_comments_and_blank_lines_skip() {
        let v = parse("; top comment\n  # indented comment\n\n[s]\na = 1\n").unwrap();
        assert_eq!(v, json!({"s": {"a": "1"}}));
    }

    #[test]
    fn sectionless_keys_land_in_default() {
        let v = parse("top = 1\n[sec]\na = 2").unwrap();
        assert_eq!(v[DEFAULT_SECTION]["top"], "1");
        assert_eq!(v["sec"]["a"], "2");
    }

    #[test]
    fn bare_key_is_null_and_empty_value_is_empty_string() {
        let v = parse("[s]\nflag\nblank =").unwrap();
        assert_eq!(v["s"]["flag"], Value::Null);
        assert_eq!(v["s"]["blank"], "");
    }

    #[test]
    fn duplicates_keep_the_last_value() {
        assert_eq!(parse("[s]\np = 1\np = 2").unwrap()["s"]["p"], "2");
    }

    #[test]
    fn empty_sections_are_kept() {
        assert_eq!(parse("[empty]\n").unwrap(), json!({"empty": {}}));
    }

    #[test]
    fn malformed_lines_error_with_line_numbers() {
        assert_eq!(parse("[ok]\n[oops\n").unwrap_err().0, 2);
        assert_eq!(parse("[s]\na = 1\n= 2").unwrap_err().0, 3);
    }
}
