use std::io;
use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{Context, Result};
use clap::Args;
use regex::Regex;

use crate::config::{Format, read_config_inputs};
use crate::output::BoundedWriter;
use crate::value::{GrepMode, TypeFilter, format_grep_path, grep};

#[derive(Args)]
#[command(
    about = "Find paths in TOML/YAML/plist whose key or value matches a regex",
    long_about = "Structural search for paths in a TOML, YAML, or plist document. \
        Unlike `fs grep` — which is text-oriented and produces false positives \
        in comments, between keys and values, or across format-specific quirks \
        — `config grep` walks the parsed document and emits dot-paths whose \
        object key (default) or scalar leaf value matches the given regex. \
        Cross-format because every input is normalized through \
        `serde_json::Value` first.\n\n\
        Output is `path<TAB>value` lines sorted by path. The empty path \
        (root scalar matches) is rendered as `(root)`. Format is auto-detected \
        from the file extension or set with `--format`. Reads from stdin if no \
        files are given (requires `--format`).",
    after_help = "\
Examples:
  sak config grep '^aws_' config.toml              Keys starting with aws_
  sak config grep -v localhost services.yaml       Values containing localhost
  sak config grep -i password secrets.yaml         Case-insensitive key match
  sak config grep '.' --type string Info.plist     All paths with a string value
  sak config grep port --paths-only ports.toml     Just the paths, no values"
)]
pub struct GrepArgs {
    /// Regex pattern to match against keys (default) or values (`--value`)
    pub pattern: String,

    /// Input files (reads stdin if omitted)
    pub files: Vec<PathBuf>,

    /// Match against object keys (default)
    #[arg(short, long, conflicts_with = "value")]
    pub key: bool,

    /// Match against scalar leaf values instead of keys
    #[arg(short, long)]
    pub value: bool,

    /// Restrict matches to values of this JSON type
    #[arg(short = 't', long = "type", value_enum)]
    pub type_filter: Option<TypeFilter>,

    /// Suppress values from output, emit only paths
    #[arg(long)]
    pub paths_only: bool,

    /// Case-insensitive matching
    #[arg(short = 'i', long = "ignore-case")]
    pub ignore_case: bool,

    /// Force a specific format (required for stdin)
    #[arg(short, long, value_enum)]
    pub format: Option<Format>,

    /// Maximum number of output lines
    #[arg(long)]
    pub limit: Option<usize>,
}

pub fn run(args: &GrepArgs) -> Result<ExitCode> {
    let inputs = read_config_inputs(&args.files, args.format)?;
    let re = build_regex(&args.pattern, args.ignore_case)?;
    let mode = if args.value {
        GrepMode::Values
    } else {
        GrepMode::Keys
    };

    let stdout = io::stdout();
    let handle = stdout.lock();
    let mut writer = BoundedWriter::new(handle, args.limit);

    let mut any = false;
    for (_name, value) in &inputs {
        for (path, val) in grep(value, &re, mode, args.type_filter) {
            any = true;
            let label = format_grep_path(&path);
            let line = if args.paths_only {
                label.to_string()
            } else {
                let rendered = serde_json::to_string(val).unwrap_or_default();
                format!("{}\t{}", label, rendered)
            };
            if !writer.write_line(&line)? {
                writer.flush()?;
                return Ok(ExitCode::SUCCESS);
            }
        }
    }

    writer.flush()?;
    if any {
        Ok(ExitCode::SUCCESS)
    } else {
        Ok(ExitCode::from(1))
    }
}

fn build_regex(pattern: &str, ignore_case: bool) -> Result<Regex> {
    let full = if ignore_case {
        format!("(?i){}", pattern)
    } else {
        pattern.to_string()
    };
    Regex::new(&full).with_context(|| format!("invalid regex: {}", pattern))
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

    fn args_with(pattern: &str, p: PathBuf) -> GrepArgs {
        GrepArgs {
            pattern: pattern.to_string(),
            files: vec![p],
            key: false,
            value: false,
            type_filter: None,
            paths_only: false,
            ignore_case: false,
            format: None,
            limit: None,
        }
    }

    #[test]
    fn grep_toml_keys() {
        let (_d, p) = write_tmp(
            "a.toml",
            "[aws]\nregion = \"us-east-1\"\nprofile = \"dev\"\n[server]\nport = 80\n",
        );
        let args = args_with("region", p);
        assert_eq!(run(&args).unwrap(), ExitCode::SUCCESS);
    }

    #[test]
    fn grep_yaml_values() {
        let (_d, p) = write_tmp("a.yaml", "host: localhost\nport: 8080\nname: web\n");
        let mut args = args_with("^local", p);
        args.value = true;
        assert_eq!(run(&args).unwrap(), ExitCode::SUCCESS);
    }

    #[test]
    fn grep_plist_keys() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleName</key>
    <string>MyApp</string>
    <key>CFBundleVersion</key>
    <string>1.0</string>
</dict>
</plist>"#;
        let (_d, p) = write_tmp("a.plist", xml);
        let args = args_with("^CFBundle", p);
        assert_eq!(run(&args).unwrap(), ExitCode::SUCCESS);
    }

    #[test]
    fn grep_no_match_exits_1() {
        let (_d, p) = write_tmp("a.toml", "a = 1\n");
        let args = args_with("nope", p);
        assert_eq!(run(&args).unwrap(), ExitCode::from(1));
    }

    #[test]
    fn grep_type_filter_strings() {
        let (_d, p) = write_tmp(
            "a.yaml",
            "items: [1, 2, 3]\nitems_label: things\nitems_count: 3\n",
        );
        let mut args = args_with("^items", p);
        args.type_filter = Some(TypeFilter::String);
        assert_eq!(run(&args).unwrap(), ExitCode::SUCCESS);
    }

    #[test]
    fn grep_paths_only() {
        let (_d, p) = write_tmp("a.toml", "[server]\nport = 8080\n");
        let mut args = args_with("port", p);
        args.paths_only = true;
        assert_eq!(run(&args).unwrap(), ExitCode::SUCCESS);
    }

    #[test]
    fn grep_ignore_case() {
        let (_d, p) = write_tmp("a.yaml", "PASSWORD: hunter2\n");
        let mut args = args_with("password", p);
        args.ignore_case = true;
        assert_eq!(run(&args).unwrap(), ExitCode::SUCCESS);
    }

    #[test]
    fn grep_invalid_regex_errors() {
        let (_d, p) = write_tmp("a.toml", "a = 1\n");
        let args = args_with("(unclosed", p);
        assert!(run(&args).is_err());
    }
}
