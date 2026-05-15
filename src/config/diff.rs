use std::io;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, Result};
use clap::Args;
use serde_json::Value;

use crate::config::{Format, detect_format, parse_one};
use crate::output::BoundedWriter;
use crate::value::{diff, format_diff_entry};

#[derive(Args)]
#[command(
    about = "Structurally diff two TOML/YAML/plist documents",
    long_about = "Compare two config documents and report added, removed, and \
        changed paths. Each file's format is auto-detected from its extension \
        independently, so a TOML file can be diffed against a YAML file when \
        they describe the same data — purely cosmetic differences in key \
        ordering, whitespace, and surface syntax are ignored.\n\n\
        Objects are compared as unordered key sets, arrays as ordered \
        positional sequences. Type mismatches at the same path are reported \
        as `Changed`.\n\n\
        Output format (one line per difference):\n\
        \n  + <path>\\t<value>            added in <b>\
        \n  - <path>\\t<value>            removed from <a>\
        \n  ~ <path>\\t<old> -> <new>     changed\n\n\
        The empty (root) path is rendered as `(root)`. Values are emitted as \
        compact JSON since both inputs are normalized through \
        `serde_json::Value` (lossy for TOML datetimes, plist dates, and plist \
        binary data — these collapse to JSON-friendly forms).\n\n\
        Exit codes follow `diff(1)` semantics, not sak's usual results-found \
        convention: 0 = identical, 1 = differences found, 2 = error.",
    after_help = "\
Examples:
  sak config diff old.toml new.toml
  sak config diff config.yaml config.toml       Cross-format diff
  sak config diff a.yaml b.yml --limit 20
  sak config diff dev.toml prod.toml && echo identical"
)]
pub struct DiffArgs {
    /// First (left) config file
    pub a: PathBuf,
    /// Second (right) config file
    pub b: PathBuf,

    /// Force the format of the first file (overrides extension)
    #[arg(long = "format-a", value_enum)]
    pub format_a: Option<Format>,

    /// Force the format of the second file (overrides extension)
    #[arg(long = "format-b", value_enum)]
    pub format_b: Option<Format>,

    /// Maximum number of output lines
    #[arg(long)]
    pub limit: Option<usize>,
}

fn load(path: &Path, override_: Option<Format>) -> Result<Value> {
    let fmt = detect_format(path, override_)?;
    let bytes = std::fs::read(path).with_context(|| format!("cannot read: {}", path.display()))?;
    parse_one(fmt, &bytes)
        .map_err(|e| anyhow::anyhow!("invalid {}: {}: {}", fmt, path.display(), e))
}

pub fn run(args: &DiffArgs) -> Result<ExitCode> {
    let a = load(&args.a, args.format_a)?;
    let b = load(&args.b, args.format_b)?;

    let entries = diff(&a, &b);
    if entries.is_empty() {
        return Ok(ExitCode::SUCCESS);
    }

    let stdout = io::stdout();
    let handle = stdout.lock();
    let mut writer = BoundedWriter::new(handle, args.limit);
    for entry in &entries {
        if !writer.write_line(&format_diff_entry(entry))? {
            break;
        }
    }
    writer.flush()?;
    Ok(ExitCode::from(1))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_tmp(name: &str, content: &[u8]) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join(name);
        let mut f = std::fs::File::create(&p).unwrap();
        f.write_all(content).unwrap();
        (dir, p)
    }

    #[test]
    fn diff_toml_identical_zero() {
        let (_d1, a) = write_tmp("a.toml", b"name = \"alice\"\nage = 30\n");
        let (_d2, b) = write_tmp("b.toml", b"age = 30\nname = \"alice\"\n");
        let args = DiffArgs {
            a,
            b,
            format_a: None,
            format_b: None,
            limit: None,
        };
        assert_eq!(run(&args).unwrap(), ExitCode::SUCCESS);
    }

    #[test]
    fn diff_toml_different_one() {
        let (_d1, a) = write_tmp("a.toml", b"port = 8080\n");
        let (_d2, b) = write_tmp("b.toml", b"port = 9090\n");
        let args = DiffArgs {
            a,
            b,
            format_a: None,
            format_b: None,
            limit: None,
        };
        assert_eq!(run(&args).unwrap(), ExitCode::from(1));
    }

    #[test]
    fn diff_cross_format_toml_vs_yaml_identical() {
        // Different syntaxes for the same logical data must diff empty.
        let (_d1, a) = write_tmp("a.toml", b"name = \"alice\"\nage = 30\n");
        let (_d2, b) = write_tmp("b.yaml", b"name: alice\nage: 30\n");
        let args = DiffArgs {
            a,
            b,
            format_a: None,
            format_b: None,
            limit: None,
        };
        assert_eq!(run(&args).unwrap(), ExitCode::SUCCESS);
    }

    #[test]
    fn diff_format_override() {
        // File has no recognized extension; format-a/-b lets it be parsed.
        let (_d1, a) = write_tmp("a.cfg", b"name = \"alice\"\n");
        let (_d2, b) = write_tmp("b.cfg", b"name = \"bob\"\n");
        let args = DiffArgs {
            a,
            b,
            format_a: Some(Format::Toml),
            format_b: Some(Format::Toml),
            limit: None,
        };
        assert_eq!(run(&args).unwrap(), ExitCode::from(1));
    }

    #[test]
    fn diff_unknown_extension_without_override_errors() {
        let (_d1, a) = write_tmp("a.cfg", b"x = 1\n");
        let (_d2, b) = write_tmp("b.cfg", b"x = 1\n");
        let args = DiffArgs {
            a,
            b,
            format_a: None,
            format_b: None,
            limit: None,
        };
        assert!(run(&args).is_err());
    }

    #[test]
    fn diff_invalid_content_errors() {
        let (_d1, a) = write_tmp("a.toml", b"name = \"alice\"\n");
        let (_d2, b) = write_tmp("b.toml", b"bad = = =\n");
        let args = DiffArgs {
            a,
            b,
            format_a: None,
            format_b: None,
            limit: None,
        };
        assert!(run(&args).is_err());
    }
}
