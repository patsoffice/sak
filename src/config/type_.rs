use crate::output::Outcome;
use std::io;
use std::path::PathBuf;

use anyhow::Result;
use clap::Args;

use crate::config::{Format, read_config_inputs};
use crate::output::BoundedWriter;
use crate::value::{resolve_expression, type_name};

#[derive(Args)]
#[command(
    about = "Print the value type at a path in a TOML, YAML, or plist document",
    long_about = "Print the JSON-equivalent type (object, array, string, number, boolean, null) \
        at a given path in a config document.\n\n\
        With no path, prints the type of the root value. Cheap discovery without \
        dumping the value itself — handy for branching agent logic on shape before \
        committing to a full extraction. Exits 1 if the path does not resolve. \
        Format is auto-detected from the file extension or set with `--format`. \
        Reads from stdin if no files are given (requires `--format`).\n\n\
        Note: TOML datetimes, plist dates, and plist binary data collapse to \
        JSON-friendly representations on parse, so their reported type is the \
        type of the collapsed value (typically `string`), not the source type.",
    after_help = "\
Examples:
  sak config type Cargo.toml                       Type of the root
  sak config type .package Cargo.toml              Type at a path
  sak config type /package/name Cargo.toml         JSON Pointer
  cat data.yaml | sak config type --format yaml    Read stdin"
)]
pub struct TypeArgs {
    /// Path within the document (default: root)
    pub path: Option<String>,

    /// Input files (reads stdin if omitted)
    pub files: Vec<PathBuf>,

    /// Force a specific format (required for stdin)
    #[arg(short, long, value_enum)]
    pub format: Option<Format>,

    /// Maximum number of output lines
    #[arg(long)]
    pub limit: Option<usize>,
}

pub fn run(args: &TypeArgs) -> Result<Outcome> {
    let inputs = read_config_inputs(&args.files, args.format)?;

    let stdout = io::stdout();
    let handle = stdout.lock();
    let mut writer = BoundedWriter::new(handle, args.limit);

    let mut found_any = false;
    for (_name, value) in &inputs {
        let target = match &args.path {
            Some(p) => match resolve_expression(value, p)? {
                Some(v) => v,
                None => continue,
            },
            None => value,
        };
        found_any = true;
        if !writer.write_line(type_name(target))? {
            writer.flush()?;
            return Ok(Outcome::Found);
        }
    }

    writer.flush()?;
    if found_any {
        Ok(Outcome::Found)
    } else {
        Ok(Outcome::NotFound)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_tmp(name: &str, content: &str) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join(name);
        let mut f = std::fs::File::create(&p).unwrap();
        f.write_all(content.as_bytes()).unwrap();
        (dir, p)
    }

    #[test]
    fn type_toml_root_is_object() {
        let (_d, p) = write_tmp("a.toml", "a = 1\nb = 2\n");
        let args = TypeArgs {
            path: None,
            files: vec![p],
            format: None,
            limit: None,
        };
        assert_eq!(run(&args).unwrap(), Outcome::Found);
    }

    #[test]
    fn type_yaml_at_path() {
        let (_d, p) = write_tmp("a.yaml", "values: [1, 2, 3]\n");
        let args = TypeArgs {
            path: Some(".values".to_string()),
            files: vec![p],
            format: None,
            limit: None,
        };
        assert_eq!(run(&args).unwrap(), Outcome::Found);
    }

    #[test]
    fn type_missing_path_returns_1() {
        let (_d, p) = write_tmp("a.toml", "a = 1\n");
        let args = TypeArgs {
            path: Some(".missing".to_string()),
            files: vec![p],
            format: None,
            limit: None,
        };
        assert_eq!(run(&args).unwrap(), Outcome::NotFound);
    }

    #[test]
    fn type_plist_pointer_syntax() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>name</key>
    <string>alice</string>
    <key>flag</key>
    <true/>
</dict>
</plist>"#;
        let (_d, p) = write_tmp("a.plist", xml);
        let args = TypeArgs {
            path: Some("/flag".to_string()),
            files: vec![p],
            format: None,
            limit: None,
        };
        assert_eq!(run(&args).unwrap(), Outcome::Found);
    }

    #[test]
    fn type_yaml_each_scalar_kind() {
        for (name, body) in [
            ("null.yaml", "~"),
            ("bool.yaml", "true"),
            ("num.yaml", "42"),
            ("str.yaml", "hello"),
            ("arr.yaml", "[]"),
            ("obj.yaml", "{}"),
        ] {
            let (_d, p) = write_tmp(name, body);
            let args = TypeArgs {
                path: None,
                files: vec![p],
                format: None,
                limit: None,
            };
            assert_eq!(run(&args).unwrap(), Outcome::Found);
        }
    }
}
