use crate::output::Outcome;
use std::collections::BTreeSet;
use std::io;
use std::path::PathBuf;

use anyhow::Result;
use clap::Args;

use crate::config::{Format, read_config_inputs};
use crate::output::BoundedWriter;
use crate::value::{SchemaMap, WalkOpts, collect_schema, format_schema_types, type_name};

#[derive(Args)]
#[command(
    about = "Infer a structural schema from a TOML/YAML/plist document",
    long_about = "Walk a TOML, YAML, or plist document and emit an inferred schema \
        as flat dot-path lines.\n\n\
        Each line has the form `path: type` (or `path: type1|type2` for unions). \
        Array elements use `[]` in the path; for example, `.servers[].host: string`. \
        When an array contains heterogeneous element types, the types are merged \
        into a union and the recursive structure of all element schemas is unioned. \
        Format is auto-detected from the file extension or set explicitly with \
        `--format`. Reads from stdin if no files are given (requires `--format`).",
    after_help = "\
Examples:
  sak config schema Cargo.toml
  sak config schema config.yaml
  sak config schema Info.plist
  sak config schema --depth 2 Cargo.toml         Limit recursion depth
  cat a.yaml | sak config schema --format yaml   Pipe stdin"
)]
pub struct SchemaArgs {
    /// Input files (reads stdin if omitted)
    pub files: Vec<PathBuf>,

    /// Maximum nesting depth (deeper structures collapse to their type only)
    #[arg(short, long)]
    pub depth: Option<usize>,

    /// Force a specific format (required for stdin)
    #[arg(short, long, value_enum)]
    pub format: Option<Format>,

    /// Maximum number of output lines
    #[arg(long)]
    pub limit: Option<usize>,
}

pub fn run(args: &SchemaArgs) -> Result<Outcome> {
    let inputs = read_config_inputs(&args.files, args.format)?;

    let stdout = io::stdout();
    let handle = stdout.lock();
    let mut writer = BoundedWriter::new(handle, args.limit);

    let mut any = false;
    for (_name, value) in &inputs {
        let mut schema = SchemaMap::new();

        // Record the root type as "(root): <type>"
        let mut root_types = BTreeSet::new();
        root_types.insert(type_name(value));
        schema.insert(String::new(), root_types);

        collect_schema(value, &WalkOpts::with_max_depth(args.depth), &mut schema);

        for (path, types) in &schema {
            any = true;
            let label = if path.is_empty() {
                "(root)".to_string()
            } else {
                path.clone()
            };
            let line = format!("{}: {}", label, format_schema_types(types));
            if !writer.write_line(&line)? {
                writer.flush()?;
                return Ok(Outcome::Found);
            }
        }
    }

    writer.flush()?;
    if any {
        Ok(Outcome::Found)
    } else {
        Ok(Outcome::NotFound)
    }
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
    fn schema_toml_basic() {
        let (_d, p) = write_tmp("a.toml", b"name = \"alice\"\nage = 30\n");
        let args = SchemaArgs {
            files: vec![p],
            depth: None,
            format: None,
            limit: None,
        };
        assert_eq!(run(&args).unwrap(), Outcome::Found);
    }

    #[test]
    fn schema_yaml_basic() {
        let (_d, p) = write_tmp("a.yaml", b"servers:\n  - host: a\n  - host: b\n");
        let args = SchemaArgs {
            files: vec![p],
            depth: None,
            format: None,
            limit: None,
        };
        assert_eq!(run(&args).unwrap(), Outcome::Found);
    }

    #[test]
    fn schema_plist_basic() {
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
        let (_d, p) = write_tmp("a.plist", xml);
        let args = SchemaArgs {
            files: vec![p],
            depth: None,
            format: None,
            limit: None,
        };
        assert_eq!(run(&args).unwrap(), Outcome::Found);
    }

    #[test]
    fn schema_yaml_array_unions() {
        // Verify the shared helper drives the same union behavior as json schema.
        let yaml = b"items:\n  - 1\n  - two\n  - true\n";
        let value: serde_json::Value = serde_yaml::from_slice(yaml).unwrap();
        let mut s = SchemaMap::new();
        let mut roots = BTreeSet::new();
        roots.insert(type_name(&value));
        s.insert(String::new(), roots);
        collect_schema(&value, &WalkOpts::with_max_depth(None), &mut s);
        let elems: Vec<&str> = s.get(".items[]").unwrap().iter().copied().collect();
        assert_eq!(elems, vec!["boolean", "number", "string"]);
    }

    #[test]
    fn schema_missing_format_for_stdin() {
        // No files + no format = error (matches config::read_config_inputs contract).
        let args = SchemaArgs {
            files: vec![],
            depth: None,
            format: None,
            limit: None,
        };
        assert!(run(&args).is_err());
    }
}
