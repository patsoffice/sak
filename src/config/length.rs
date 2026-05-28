use crate::output::Outcome;
use std::io;
use std::path::PathBuf;

use anyhow::{Result, anyhow};
use clap::Args;

use crate::config::{Format, read_config_inputs};
use crate::output::BoundedWriter;
use crate::value::{resolve_expression, type_name, value_length};

#[derive(Args)]
#[command(
    about = "Print the length of the value at a path in a TOML, YAML, or plist document",
    long_about = "Print the length of the value at a path: element count for arrays, \
        key count for objects/tables, Unicode scalar (char) count for strings.\n\n\
        Errors on number, boolean, and null values — these have no meaningful \
        length. Exits 1 if the path does not resolve. With no path, the root \
        value is measured. Format is auto-detected from the file extension or \
        set with `--format`. Reads from stdin if no files are given (requires \
        `--format`).\n\n\
        Note: TOML datetimes, plist dates, and plist binary data collapse to \
        JSON-friendly representations on parse, so their length is the length \
        of the collapsed value (typically the string form), not the source type.",
    after_help = "\
Examples:
  sak config length .dependencies Cargo.toml        Count dependencies
  sak config length .package.name Cargo.toml        Length of the name string
  sak config length /tools/0 pyproject.toml         JSON Pointer
  cat values.yaml | sak config length --format yaml Length of root"
)]
pub struct LengthArgs {
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

pub fn run(args: &LengthArgs) -> Result<Outcome> {
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
        let len = value_length(target).ok_or_else(|| {
            anyhow!(
                "value of type '{}' has no length (expected array, object, or string)",
                type_name(target)
            )
        })?;
        if !writer.write_line(&len.to_string())? {
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
    fn length_toml_root_is_key_count() {
        let (_d, p) = write_tmp("a.toml", "a = 1\nb = 2\nc = 3\n");
        let args = LengthArgs {
            path: None,
            files: vec![p],
            format: None,
            limit: None,
        };
        assert_eq!(run(&args).unwrap(), Outcome::Found);
    }

    #[test]
    fn length_yaml_array_at_path() {
        let (_d, p) = write_tmp("a.yaml", "values: [1, 2, 3, 4]\n");
        let args = LengthArgs {
            path: Some(".values".to_string()),
            files: vec![p],
            format: None,
            limit: None,
        };
        assert_eq!(run(&args).unwrap(), Outcome::Found);
    }

    #[test]
    fn length_yaml_string_chars() {
        let (_d, p) = write_tmp("a.yaml", "name: hello\n");
        let args = LengthArgs {
            path: Some(".name".to_string()),
            files: vec![p],
            format: None,
            limit: None,
        };
        assert_eq!(run(&args).unwrap(), Outcome::Found);
    }

    #[test]
    fn length_missing_path_returns_1() {
        let (_d, p) = write_tmp("a.toml", "a = 1\n");
        let args = LengthArgs {
            path: Some(".missing".to_string()),
            files: vec![p],
            format: None,
            limit: None,
        };
        assert_eq!(run(&args).unwrap(), Outcome::NotFound);
    }

    #[test]
    fn length_of_scalar_errors() {
        let (_d, p) = write_tmp("a.toml", "port = 8080\n");
        let args = LengthArgs {
            path: Some(".port".to_string()),
            files: vec![p],
            format: None,
            limit: None,
        };
        assert!(run(&args).is_err());
    }

    #[test]
    fn length_plist_pointer_syntax() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>items</key>
    <array>
        <string>a</string>
        <string>b</string>
        <string>c</string>
    </array>
</dict>
</plist>"#;
        let (_d, p) = write_tmp("a.plist", xml);
        let args = LengthArgs {
            path: Some("/items".to_string()),
            files: vec![p],
            format: None,
            limit: None,
        };
        assert_eq!(run(&args).unwrap(), Outcome::Found);
    }
}
