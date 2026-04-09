//! `config` domain — read-only operations on TOML, YAML, and plist files.
//!
//! Every command in this module parses inputs into `serde_json::Value` so that
//! the format-agnostic helpers in [`crate::value`] can do the actual work. This
//! is intentionally lossy at the edges (TOML datetimes, plist dates, plist
//! binary data all collapse to JSON-friendly representations) — acceptable for
//! an LLM-facing read-only tool.

pub mod flatten;
pub mod keys;
pub mod query;
pub mod validate;

use std::ffi::OsStr;
use std::fmt;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, Result, bail};
use clap::{Subcommand, ValueEnum};
use serde_json::Value;

#[derive(Subcommand)]
pub enum ConfigCommand {
    Query(query::QueryArgs),
    Keys(keys::KeysArgs),
    Flatten(flatten::FlattenArgs),
    Validate(validate::ValidateArgs),
}

pub fn run(cmd: &ConfigCommand) -> Result<ExitCode> {
    match cmd {
        ConfigCommand::Query(args) => query::run(args),
        ConfigCommand::Keys(args) => keys::run(args),
        ConfigCommand::Flatten(args) => flatten::run(args),
        ConfigCommand::Validate(args) => validate::run(args),
    }
}

/// The structured config file formats this domain understands.
#[derive(Copy, Clone, Debug, PartialEq, Eq, ValueEnum)]
pub enum Format {
    Toml,
    Yaml,
    Plist,
}

impl fmt::Display for Format {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Format::Toml => "toml",
            Format::Yaml => "yaml",
            Format::Plist => "plist",
        })
    }
}

/// Parse failure with optional position info, normalized across all parsers.
#[derive(Debug)]
pub struct ParseError {
    pub line: Option<usize>,
    pub col: Option<usize>,
    pub message: String,
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match (self.line, self.col) {
            (Some(l), Some(c)) => write!(f, "{}:{}: {}", l, c, self.message),
            (Some(l), None) => write!(f, "{}: {}", l, self.message),
            _ => f.write_str(&self.message),
        }
    }
}

/// Detect a file's format from its extension. An explicit override always wins.
pub fn detect_format(path: &Path, override_: Option<Format>) -> Result<Format> {
    if let Some(f) = override_ {
        return Ok(f);
    }
    let ext = path
        .extension()
        .and_then(OsStr::to_str)
        .map(str::to_lowercase);
    match ext.as_deref() {
        Some("toml") => Ok(Format::Toml),
        Some("yaml" | "yml") => Ok(Format::Yaml),
        Some("plist") => Ok(Format::Plist),
        _ => bail!(
            "cannot detect format for {} — pass --format toml|yaml|plist",
            path.display()
        ),
    }
}

/// Parse a single document into `serde_json::Value`. Errors are normalized to
/// [`ParseError`] so callers (especially `validate`) get uniform position info.
pub fn parse_one(format: Format, content: &[u8]) -> std::result::Result<Value, ParseError> {
    match format {
        Format::Toml => {
            let s = std::str::from_utf8(content).map_err(|e| ParseError {
                line: None,
                col: None,
                message: format!("not valid UTF-8: {}", e),
            })?;
            toml::from_str::<Value>(s).map_err(|e| {
                let (line, col) = e
                    .span()
                    .and_then(|span| line_col(s, span.start))
                    .map(|(l, c)| (Some(l), Some(c)))
                    .unwrap_or((None, None));
                ParseError {
                    line,
                    col,
                    message: e.message().to_string(),
                }
            })
        }
        Format::Yaml => {
            let s = std::str::from_utf8(content).map_err(|e| ParseError {
                line: None,
                col: None,
                message: format!("not valid UTF-8: {}", e),
            })?;
            serde_yaml::from_str::<Value>(s).map_err(|e| {
                let loc = e.location();
                ParseError {
                    line: loc.as_ref().map(|l| l.line()),
                    col: loc.as_ref().map(|l| l.column()),
                    message: e.to_string(),
                }
            })
        }
        Format::Plist => plist::from_bytes::<Value>(content).map_err(|e| ParseError {
            line: None,
            col: None,
            message: e.to_string(),
        }),
    }
}

/// Read inputs from files (or stdin), auto-detecting format per file unless
/// `format` overrides it. Stdin requires an explicit format.
pub fn read_config_inputs(
    files: &[PathBuf],
    format: Option<Format>,
) -> Result<Vec<(String, Value)>> {
    let mut out = Vec::new();
    if files.is_empty() {
        let fmt = format.context("--format is required when reading from stdin")?;
        let mut buf = Vec::new();
        io::stdin()
            .read_to_end(&mut buf)
            .context("error reading stdin")?;
        let value =
            parse_one(fmt, &buf).map_err(|e| anyhow::anyhow!("invalid {} on stdin: {}", fmt, e))?;
        out.push(("<stdin>".to_string(), value));
    } else {
        for path in files {
            let fmt = detect_format(path, format)?;
            let bytes =
                std::fs::read(path).with_context(|| format!("cannot read: {}", path.display()))?;
            let value = parse_one(fmt, &bytes)
                .map_err(|e| anyhow::anyhow!("invalid {}: {}: {}", fmt, path.display(), e))?;
            out.push((path.display().to_string(), value));
        }
    }
    Ok(out)
}

/// Convert a byte offset in `s` into a 1-based (line, column) pair.
fn line_col(s: &str, offset: usize) -> Option<(usize, usize)> {
    if offset > s.len() {
        return None;
    }
    let mut line = 1usize;
    let mut col = 1usize;
    for (i, ch) in s.char_indices() {
        if i >= offset {
            return Some((line, col));
        }
        if ch == '\n' {
            line += 1;
            col = 1;
        } else {
            col += 1;
        }
    }
    Some((line, col))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_by_extension() {
        assert_eq!(
            detect_format(Path::new("a.toml"), None).unwrap(),
            Format::Toml
        );
        assert_eq!(
            detect_format(Path::new("a.yaml"), None).unwrap(),
            Format::Yaml
        );
        assert_eq!(
            detect_format(Path::new("a.yml"), None).unwrap(),
            Format::Yaml
        );
        assert_eq!(
            detect_format(Path::new("a.plist"), None).unwrap(),
            Format::Plist
        );
    }

    #[test]
    fn detect_unknown_errors() {
        assert!(detect_format(Path::new("a.txt"), None).is_err());
    }

    #[test]
    fn detect_override_wins() {
        assert_eq!(
            detect_format(Path::new("a.txt"), Some(Format::Yaml)).unwrap(),
            Format::Yaml
        );
    }

    #[test]
    fn parse_toml_basic() {
        let v = parse_one(Format::Toml, b"name = \"alice\"\nage = 30").unwrap();
        assert_eq!(v["name"], "alice");
        assert_eq!(v["age"], 30);
    }

    #[test]
    fn parse_yaml_basic() {
        let v = parse_one(Format::Yaml, b"name: alice\nage: 30").unwrap();
        assert_eq!(v["name"], "alice");
        assert_eq!(v["age"], 30);
    }

    #[test]
    fn parse_plist_xml() {
        let xml = br#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>name</key>
    <string>alice</string>
    <key>age</key>
    <integer>30</integer>
</dict>
</plist>"#;
        let v = parse_one(Format::Plist, xml).unwrap();
        assert_eq!(v["name"], "alice");
        assert_eq!(v["age"], 30);
    }

    #[test]
    fn parse_toml_invalid_reports_position() {
        let err = parse_one(Format::Toml, b"a = =").unwrap_err();
        assert!(err.line.is_some());
    }
}
