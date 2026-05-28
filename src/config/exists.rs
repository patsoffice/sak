use crate::output::Outcome;
use std::path::PathBuf;

use anyhow::Result;
use clap::Args;

use crate::config::{Format, read_config_inputs};
use crate::value::resolve_expression;

#[derive(Args)]
#[command(
    about = "Check whether a path exists in TOML, YAML, or plist",
    long_about = "Boolean check for whether a path exists in a config document.\n\n\
        Produces no stdout output — the result is signalled by the exit code: \
        0 if the path exists, 1 if it is missing. Useful in agent loops and \
        shell pipelines where the value itself isn't needed.\n\n\
        Format is auto-detected from the file extension (.toml, .yaml/.yml, .plist) \
        or may be set explicitly with `--format`. The expression accepts the same \
        dot notation (e.g. `.server.port`) and JSON Pointer syntax (e.g. `/server/port`) \
        as `sak config query`. Reads from stdin if no files are given (requires `--format`).\n\n\
        With multiple files, the default is *all*: exit 0 only if every input \
        contains the path. Pass `--any` to flip the semantics (exit 0 if any \
        input contains the path).",
    after_help = "\
Examples:
  sak config exists .features.beta config.toml && enable_beta
  sak config exists .server.port config.yaml
  sak config exists .CFBundleName Info.plist
  sak config exists .api.key dev.toml prod.toml      All files must contain it
  sak config exists --any .deprecated *.toml         Any file containing it
  echo 'a: 1' | sak config exists .a --format yaml   Read from stdin"
)]
pub struct ExistsArgs {
    /// Path expression (dot notation or JSON Pointer)
    pub expression: String,

    /// Input files (reads stdin if omitted)
    pub files: Vec<PathBuf>,

    /// Force a specific format (required for stdin)
    #[arg(short, long, value_enum)]
    pub format: Option<Format>,

    /// Exit 0 if the path exists in *any* input (default: requires *all*)
    #[arg(long)]
    pub any: bool,
}

pub fn run(args: &ExistsArgs) -> Result<Outcome> {
    let inputs = read_config_inputs(&args.files, args.format)?;

    let mut all_present = true;
    let mut any_present = false;
    for (_name, value) in &inputs {
        if resolve_expression(value, &args.expression)?.is_some() {
            any_present = true;
        } else {
            all_present = false;
        }
    }

    let exists = if args.any { any_present } else { all_present };
    if exists {
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
    fn exists_toml_present() {
        let (_d, p) = write_tmp("a.toml", "name = \"alice\"\n");
        let args = ExistsArgs {
            expression: ".name".to_string(),
            files: vec![p],
            format: None,
            any: false,
        };
        assert_eq!(run(&args).unwrap(), Outcome::Found);
    }

    #[test]
    fn exists_yaml_present() {
        let (_d, p) = write_tmp("a.yaml", "server:\n  port: 8080\n");
        let args = ExistsArgs {
            expression: ".server.port".to_string(),
            files: vec![p],
            format: None,
            any: false,
        };
        assert_eq!(run(&args).unwrap(), Outcome::Found);
    }

    #[test]
    fn exists_plist_present() {
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
        let args = ExistsArgs {
            expression: ".name".to_string(),
            files: vec![p],
            format: None,
            any: false,
        };
        assert_eq!(run(&args).unwrap(), Outcome::Found);
    }

    #[test]
    fn exists_missing_returns_1() {
        let (_d, p) = write_tmp("a.toml", "a = 1\n");
        let args = ExistsArgs {
            expression: ".missing".to_string(),
            files: vec![p],
            format: None,
            any: false,
        };
        assert_eq!(run(&args).unwrap(), Outcome::NotFound);
    }

    #[test]
    fn exists_json_pointer() {
        let (_d, p) = write_tmp("a.toml", "[server]\nport = 8080\n");
        let args = ExistsArgs {
            expression: "/server/port".to_string(),
            files: vec![p],
            format: None,
            any: false,
        };
        assert_eq!(run(&args).unwrap(), Outcome::Found);
    }

    #[test]
    fn exists_all_requires_every_file() {
        let (_d1, p1) = write_tmp("a.toml", "a = 1\n");
        let dir2 = tempfile::tempdir().unwrap();
        let p2 = dir2.path().join("b.toml");
        std::fs::File::create(&p2)
            .unwrap()
            .write_all(b"b = 2\n")
            .unwrap();
        let args = ExistsArgs {
            expression: ".a".to_string(),
            files: vec![p1, p2],
            format: None,
            any: false,
        };
        assert_eq!(run(&args).unwrap(), Outcome::NotFound);
    }

    #[test]
    fn exists_any_only_requires_one_file() {
        let (_d1, p1) = write_tmp("a.toml", "a = 1\n");
        let dir2 = tempfile::tempdir().unwrap();
        let p2 = dir2.path().join("b.toml");
        std::fs::File::create(&p2)
            .unwrap()
            .write_all(b"b = 2\n")
            .unwrap();
        let args = ExistsArgs {
            expression: ".a".to_string(),
            files: vec![p1, p2],
            format: None,
            any: true,
        };
        assert_eq!(run(&args).unwrap(), Outcome::Found);
    }
}
