use crate::output::Outcome;
use std::io;
use std::path::PathBuf;

use anyhow::Result;
use clap::Args;
use serde_json::Value;

use crate::config::{Format, read_config_inputs};
use crate::json::select::{Field, parse_fields, project};
use crate::output::BoundedWriter;

#[derive(Args)]
#[command(
    about = "Project a subset of fields from a TOML/YAML/plist/JSON object",
    long_about = "Project a subset of fields from a config object — the SQL `SELECT` of \
        the config domain. Each requested path becomes one key in a new \
        object; by default the key is the path's last segment, but an \
        explicit alias may be given with `alias=path`.\n\n\
        Inputs parse into `serde_json::Value` first (TOML datetimes and plist \
        dates collapse to JSON-friendly representations), so paths use the \
        same dot notation (`.server.port`) or JSON Pointer (`/server/port`) \
        as `sak json select`, and projection, aliasing, `--null-missing`, \
        and output sorting behave identically. Multiple paths are \
        comma-separated. The root must be an object (non-object roots produce \
        no output and exit 1). Missing paths are silently omitted unless \
        `--null-missing` is set.\n\n\
        Format is auto-detected from the file extension or set explicitly with \
        `--format` (required for stdin).",
    after_help = "\
Examples:
  sak config select .package.name,.package.version Cargo.toml
  sak config select 'id=.user.id,name=.user.name' config.yaml   Alias nested paths
  sak config select --null-missing .a,.b config.yaml            Missing paths -> null
  sak config select --pretty .name,.age Info.plist              Pretty-print output"
)]
pub struct SelectArgs {
    /// Comma-separated list of paths to project (each `path` or `alias=path`)
    pub paths: String,

    /// Input files (reads stdin if omitted or given as "-")
    pub files: Vec<PathBuf>,

    /// Force a specific format (required for stdin)
    #[arg(short, long, value_enum)]
    pub format: Option<Format>,

    /// Include missing paths as JSON null instead of omitting them
    #[arg(long)]
    pub null_missing: bool,

    /// Pretty-print output
    #[arg(long)]
    pub pretty: bool,

    /// Maximum number of output lines
    #[arg(long)]
    pub limit: Option<usize>,
}

/// Project every object input to its output lines. Separated from [`run`]
/// so tests can assert the projected object instead of only the exit
/// [`Outcome`]; [`run`] only adds stdout emission (and `--limit`) on top.
fn selected_lines(
    inputs: &[(String, Value)],
    fields: &[Field],
    null_missing: bool,
    pretty: bool,
) -> Result<Vec<String>> {
    let mut lines = Vec::new();
    for (_name, value) in inputs {
        if !value.is_object() {
            continue;
        }
        let projected = project(value, fields, null_missing)?;
        if projected.is_empty() && !null_missing {
            continue;
        }
        let out = Value::Object(projected);
        let formatted = if pretty {
            serde_json::to_string_pretty(&out).unwrap_or_default()
        } else {
            serde_json::to_string(&out).unwrap_or_default()
        };
        lines.extend(formatted.split('\n').map(str::to_string));
    }
    Ok(lines)
}

pub fn run(args: &SelectArgs) -> Result<Outcome> {
    let fields = parse_fields(&args.paths)?;
    let inputs = read_config_inputs(&args.files, args.format)?;
    let lines = selected_lines(&inputs, &fields, args.null_missing, args.pretty)?;

    let stdout = io::stdout();
    let handle = stdout.lock();
    let mut writer = BoundedWriter::new(handle, args.limit);

    for line in &lines {
        if !writer.write_line(line)? {
            break;
        }
    }

    writer.flush()?;
    if lines.is_empty() {
        Ok(Outcome::NotFound)
    } else {
        Ok(Outcome::Found)
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

    fn args_for(paths: &str, p: PathBuf) -> SelectArgs {
        SelectArgs {
            paths: paths.to_string(),
            files: vec![p],
            format: None,
            null_missing: false,
            pretty: false,
            limit: None,
        }
    }

    #[test]
    fn run_toml_projects() {
        let (_d, p) = write_tmp("a.toml", "name = \"alice\"\nage = 30\ncity = \"nyc\"\n");
        assert_eq!(run(&args_for(".name,.age", p)).unwrap(), Outcome::Found);
    }

    #[test]
    fn run_yaml_alias_and_missing() {
        let (_d, p) = write_tmp("a.yaml", "user:\n  id: 7\n  name: alice\n");
        let fields = parse_fields("id=.user.id,name=.user.name,nope=.nope").unwrap();
        let inputs = read_config_inputs(std::slice::from_ref(&p), None).unwrap();
        // Aliases rename, missing is silently omitted without --null-missing.
        assert_eq!(
            selected_lines(&inputs, &fields, false, false).unwrap(),
            vec![r#"{"id":7,"name":"alice"}"#.to_string()]
        );
        // ... and surfaces as null with --null-missing.
        assert_eq!(
            selected_lines(&inputs, &fields, true, false).unwrap(),
            vec![r#"{"id":7,"name":"alice","nope":null}"#.to_string()]
        );
        let mut args = args_for("id=.user.id,name=.user.name,nope=.nope", p);
        assert_eq!(run(&args).unwrap(), Outcome::Found);
        args.null_missing = true;
        assert_eq!(run(&args).unwrap(), Outcome::Found);
    }

    #[test]
    fn run_non_object_returns_1() {
        // A bare YAML array parses to a non-object root, which select skips.
        let (_d, p) = write_tmp("a.yaml", "- 1\n- 2\n");
        assert_eq!(run(&args_for(".name", p)).unwrap(), Outcome::NotFound);
    }

    #[test]
    fn run_all_missing_returns_1() {
        let (_d, p) = write_tmp("a.yaml", "a: 1\n");
        assert_eq!(
            run(&args_for(".missing,.nope", p)).unwrap(),
            Outcome::NotFound
        );
    }
}
