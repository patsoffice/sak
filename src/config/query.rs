use std::io;
use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::Result;
use clap::Args;

use crate::config::{Format, read_config_inputs};
use crate::output::BoundedWriter;
use crate::value::{format_value, resolve_expression};

#[derive(Args)]
#[command(
    about = "Extract values from TOML, YAML, or plist",
    long_about = "Extract values from a config file using a path expression.\n\n\
        Format is auto-detected from the file extension (.toml, .yaml/.yml, .plist) \
        or may be set explicitly with `--format`. The expression may use dot notation \
        (e.g. `.server.port`) or JSON Pointer syntax (e.g. `/server/port`). \
        Reads from stdin if no files are given (requires `--format`).",
    after_help = "\
Examples:
  sak config query .package.name Cargo.toml
  sak config query .server.port config.yaml
  sak config query .CFBundleName Info.plist
  sak config query .name --raw config.toml         Raw string output
  echo 'a: 1' | sak config query .a --format yaml  Read from stdin"
)]
pub struct QueryArgs {
    /// Path expression (dot notation or JSON Pointer)
    pub expression: String,

    /// Input files (reads stdin if omitted)
    pub files: Vec<PathBuf>,

    /// Output raw strings without surrounding quotes
    #[arg(short, long)]
    pub raw: bool,

    /// Compact output (default)
    #[arg(long, conflicts_with = "pretty")]
    pub compact: bool,

    /// Pretty-print output
    #[arg(long)]
    pub pretty: bool,

    /// Force a specific format (required for stdin)
    #[arg(short, long, value_enum)]
    pub format: Option<Format>,

    /// Maximum number of output lines
    #[arg(long)]
    pub limit: Option<usize>,
}

pub fn run(args: &QueryArgs) -> Result<ExitCode> {
    let inputs = read_config_inputs(&args.files, args.format)?;

    let stdout = io::stdout();
    let handle = stdout.lock();
    let mut writer = BoundedWriter::new(handle, args.limit);

    let mut found_any = false;
    for (_name, value) in &inputs {
        if let Some(result) = resolve_expression(value, &args.expression)? {
            found_any = true;
            let formatted = format_value(result, args.raw, args.pretty);
            for line in formatted.split('\n') {
                if !writer.write_line(line)? {
                    writer.flush()?;
                    return Ok(ExitCode::SUCCESS);
                }
            }
        }
    }

    writer.flush()?;
    if found_any {
        Ok(ExitCode::SUCCESS)
    } else {
        Ok(ExitCode::from(1))
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
    fn query_toml() {
        let (_d, p) = write_tmp("a.toml", "name = \"alice\"\n");
        let args = QueryArgs {
            expression: ".name".to_string(),
            files: vec![p],
            raw: false,
            compact: false,
            pretty: false,
            format: None,
            limit: None,
        };
        assert_eq!(run(&args).unwrap(), ExitCode::SUCCESS);
    }

    #[test]
    fn query_yaml() {
        let (_d, p) = write_tmp("a.yaml", "server:\n  port: 8080\n");
        let args = QueryArgs {
            expression: ".server.port".to_string(),
            files: vec![p],
            raw: false,
            compact: false,
            pretty: false,
            format: None,
            limit: None,
        };
        assert_eq!(run(&args).unwrap(), ExitCode::SUCCESS);
    }

    #[test]
    fn query_plist() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>name</key>
    <string>alice</string>
</dict>
</plist>
"#;
        let (_d, p) = write_tmp("a.plist", xml);
        let args = QueryArgs {
            expression: ".name".to_string(),
            files: vec![p],
            raw: false,
            compact: false,
            pretty: false,
            format: None,
            limit: None,
        };
        assert_eq!(run(&args).unwrap(), ExitCode::SUCCESS);
    }

    #[test]
    fn query_missing_returns_1() {
        let (_d, p) = write_tmp("a.toml", "a = 1\n");
        let args = QueryArgs {
            expression: ".missing".to_string(),
            files: vec![p],
            raw: false,
            compact: false,
            pretty: false,
            format: None,
            limit: None,
        };
        assert_eq!(run(&args).unwrap(), ExitCode::from(1));
    }
}
