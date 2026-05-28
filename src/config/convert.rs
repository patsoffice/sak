use crate::output::Outcome;
use std::io::{self, Read, Write};
use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::Args;
use serde_json::Value;

use crate::config::{Format, detect_format, parse_one};

#[derive(Args)]
#[command(
    about = "Convert TOML, YAML, plist, or JSON to a different format",
    long_about = "Convert a structured config document between TOML, YAML, plist, and JSON.\n\n\
        Source format is auto-detected from each file's extension or set explicitly with \
        `--from`. Target format is required and given with `--to`. Output goes to stdout; \
        source files are never touched. Reads from stdin if no files are given \
        (requires `--from`). When multiple files are given, each is converted in turn and \
        the outputs are concatenated, with `---` separators inserted between YAML documents \
        so the combined output remains parseable as multi-document YAML.\n\n\
        All formats round-trip through `serde_json::Value`, which is intentionally lossy at \
        the edges: TOML datetimes become RFC 3339 strings, plist Date values become strings, \
        plist Data values become base64-ish strings, and any type not representable in the \
        target format (e.g. JSON `null` in TOML — TOML has no null) will produce a serializer \
        error rather than silently drop the value. TOML output additionally requires the \
        document root to be a table, since TOML has no top-level array or scalar form.",
    after_help = "\
Examples:
  sak config convert --to yaml Cargo.toml
  sak config convert --to json --pretty config.toml
  sak config convert --to toml config.yaml > config.toml
  sak config convert --to json Info.plist           Binary plist -> JSON
  cat a.yaml | sak config convert --from yaml --to json
  sak config convert --to yaml --from json a.json"
)]
pub struct ConvertArgs {
    /// Input files (reads stdin if omitted)
    pub files: Vec<PathBuf>,

    /// Target format
    #[arg(short, long, value_enum)]
    pub to: Format,

    /// Source format (required for stdin; auto-detected from extension otherwise)
    #[arg(short, long, value_enum)]
    pub from: Option<Format>,

    /// Pretty-print JSON output (default; no effect on YAML, TOML, or plist)
    #[arg(long, conflicts_with = "compact")]
    pub pretty: bool,

    /// Compact JSON output on a single line (no effect on YAML, TOML, or plist)
    #[arg(long, conflicts_with = "pretty")]
    pub compact: bool,
}

pub fn run(args: &ConvertArgs) -> Result<Outcome> {
    let stdout = io::stdout();
    let mut handle = stdout.lock();

    // `--compact` flips pretty off; default for JSON is pretty.
    let pretty_json = !args.compact;

    if args.files.is_empty() {
        let fmt = args
            .from
            .context("--from is required when reading from stdin")?;
        let mut buf = Vec::new();
        io::stdin()
            .read_to_end(&mut buf)
            .context("error reading stdin")?;
        let value =
            parse_one(fmt, &buf).map_err(|e| anyhow::anyhow!("invalid {} on stdin: {}", fmt, e))?;
        emit(&value, args.to, pretty_json, &mut handle)?;
    } else {
        for (i, path) in args.files.iter().enumerate() {
            let fmt = detect_format(path, args.from)?;
            let bytes =
                std::fs::read(path).with_context(|| format!("cannot read: {}", path.display()))?;
            let value = parse_one(fmt, &bytes)
                .map_err(|e| anyhow::anyhow!("invalid {}: {}: {}", fmt, path.display(), e))?;
            if i > 0 && args.to == Format::Yaml {
                handle.write_all(b"---\n")?;
            }
            emit(&value, args.to, pretty_json, &mut handle)?;
        }
    }

    handle.flush()?;
    Ok(Outcome::Found)
}

/// Serialize `value` as `target` to `w`, ensuring the output ends with a single newline.
fn emit<W: Write>(value: &Value, target: Format, pretty_json: bool, w: &mut W) -> Result<()> {
    match target {
        Format::Json => {
            let s = if pretty_json {
                serde_json::to_string_pretty(value)
            } else {
                serde_json::to_string(value)
            }
            .context("cannot serialize to JSON")?;
            w.write_all(s.as_bytes())?;
            if !s.ends_with('\n') {
                w.write_all(b"\n")?;
            }
        }
        Format::Toml => {
            let s = toml::to_string_pretty(value).context("cannot serialize to TOML")?;
            w.write_all(s.as_bytes())?;
            if !s.ends_with('\n') {
                w.write_all(b"\n")?;
            }
        }
        Format::Yaml => {
            // serde_yaml::to_string already terminates the document with '\n'.
            let s = serde_yaml::to_string(value).context("cannot serialize to YAML")?;
            w.write_all(s.as_bytes())?;
            if !s.ends_with('\n') {
                w.write_all(b"\n")?;
            }
        }
        Format::Plist => {
            // plist::to_writer_xml writes the XML doctype + closing tag and ends with '\n'.
            plist::to_writer_xml(&mut *w, value).context("cannot serialize to plist")?;
            w.write_all(b"\n")?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn write_tmp(name: &str, content: &[u8]) -> (tempfile::TempDir, PathBuf) {
        use std::io::Write as _;
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join(name);
        let mut f = std::fs::File::create(&p).unwrap();
        f.write_all(content).unwrap();
        (dir, p)
    }

    /// Convert in-memory without going through stdout, so tests can assert on the bytes.
    fn convert_bytes(input: &[u8], from: Format, to: Format, pretty_json: bool) -> Vec<u8> {
        let value = parse_one(from, input).unwrap();
        let mut buf = Vec::new();
        emit(&value, to, pretty_json, &mut buf).unwrap();
        buf
    }

    #[test]
    fn toml_to_yaml() {
        let out = convert_bytes(
            b"name = \"alice\"\nage = 30\n",
            Format::Toml,
            Format::Yaml,
            true,
        );
        let s = std::str::from_utf8(&out).unwrap();
        assert!(s.contains("name: alice"));
        assert!(s.contains("age: 30"));
        assert!(s.ends_with('\n'));
    }

    #[test]
    fn yaml_to_toml() {
        let out = convert_bytes(b"name: alice\nage: 30\n", Format::Yaml, Format::Toml, true);
        let s = std::str::from_utf8(&out).unwrap();
        assert!(s.contains("name = \"alice\""));
        assert!(s.contains("age = 30"));
        assert!(s.ends_with('\n'));
    }

    #[test]
    fn json_to_yaml() {
        let out = convert_bytes(
            b"{\"name\":\"alice\",\"age\":30}",
            Format::Json,
            Format::Yaml,
            true,
        );
        let s = std::str::from_utf8(&out).unwrap();
        assert!(s.contains("name: alice"));
        assert!(s.contains("age: 30"));
    }

    #[test]
    fn toml_to_json_pretty_default() {
        let out = convert_bytes(b"name = \"alice\"\n", Format::Toml, Format::Json, true);
        let s = std::str::from_utf8(&out).unwrap();
        assert!(s.contains("\n  \"name\": \"alice\""));
        assert!(s.ends_with('\n'));
    }

    #[test]
    fn toml_to_json_compact() {
        let out = convert_bytes(b"name = \"alice\"\n", Format::Toml, Format::Json, false);
        let s = std::str::from_utf8(&out).unwrap();
        assert_eq!(s.trim_end(), "{\"name\":\"alice\"}");
    }

    #[test]
    fn yaml_to_plist_xml() {
        let out = convert_bytes(b"name: alice\nage: 30\n", Format::Yaml, Format::Plist, true);
        let s = std::str::from_utf8(&out).unwrap();
        assert!(s.contains("<?xml"));
        assert!(s.contains("<key>name</key>"));
        assert!(s.contains("<string>alice</string>"));
        assert!(s.contains("</plist>"));
    }

    #[test]
    fn plist_xml_to_json_roundtrip_value() {
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
        let out = convert_bytes(xml, Format::Plist, Format::Json, false);
        let s = std::str::from_utf8(&out).unwrap();
        let v: Value = serde_json::from_str(s).unwrap();
        assert_eq!(v["name"], "alice");
        assert_eq!(v["age"], 30);
    }

    #[test]
    fn yaml_to_yaml_passthrough_via_value() {
        // Round-tripping YAML through serde_json::Value is lossy for things like
        // YAML-specific tags, but for plain scalars/maps/arrays the document
        // should be structurally identical.
        let out = convert_bytes(b"items:\n  - 1\n  - 2\n", Format::Yaml, Format::Yaml, true);
        let s = std::str::from_utf8(&out).unwrap();
        assert!(s.contains("items:"));
        assert!(s.contains("- 1"));
        assert!(s.contains("- 2"));
    }

    #[test]
    fn toml_top_level_array_errors() {
        // JSON allows a top-level array; TOML does not. The serializer should
        // surface the error with context, not panic.
        let value = parse_one(Format::Json, b"[1, 2, 3]").unwrap();
        let mut buf = Vec::new();
        let err = emit(&value, Format::Toml, true, &mut buf).unwrap_err();
        let msg = format!("{:#}", err);
        assert!(msg.contains("TOML"), "unexpected error: {}", msg);
    }

    #[test]
    fn run_converts_file_to_yaml_via_extension() {
        // End-to-end: writes to stdout. We can't capture stdout here, but we
        // can at least verify the run path returns success on a real file.
        let (_d, p) = write_tmp("a.toml", b"name = \"alice\"\n");
        let args = ConvertArgs {
            files: vec![p],
            to: Format::Yaml,
            from: None,
            pretty: false,
            compact: false,
        };
        assert_eq!(run(&args).unwrap(), Outcome::Found);
    }

    #[test]
    fn run_errors_without_from_on_stdin() {
        let args = ConvertArgs {
            files: vec![],
            to: Format::Json,
            from: None,
            pretty: false,
            compact: false,
        };
        assert!(run(&args).is_err());
    }

    #[test]
    fn multi_file_yaml_inserts_separator() {
        // Emit two YAML docs into a single buffer the same way `run` would.
        let v1 = parse_one(Format::Toml, b"a = 1\n").unwrap();
        let v2 = parse_one(Format::Toml, b"b = 2\n").unwrap();
        let mut buf = Vec::new();
        emit(&v1, Format::Yaml, true, &mut buf).unwrap();
        buf.write_all(b"---\n").unwrap();
        emit(&v2, Format::Yaml, true, &mut buf).unwrap();
        let s = std::str::from_utf8(&buf).unwrap();
        assert!(s.contains("\n---\n"));
        assert!(s.contains("a: 1"));
        assert!(s.contains("b: 2"));
    }
}
