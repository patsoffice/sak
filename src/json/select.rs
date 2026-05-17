use std::io;
use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{Result, bail};
use clap::Args;
use serde_json::{Map, Value};

use crate::json::read_json_inputs_maybe_lines;
use crate::output::BoundedWriter;
use crate::value::resolve_expression;

#[derive(Args)]
#[command(
    about = "Project a subset of fields from a JSON object",
    long_about = "Project a subset of fields from a JSON object — the SQL `SELECT` of \
        the json domain. Each requested path becomes one key in a new \
        object; by default the key is the path's last segment, but an \
        explicit alias may be given with `alias=path`.\n\n\
        Paths use dot notation (`.user.name`) or JSON Pointer (`/user/name`). \
        Multiple paths are comma-separated. The root of the input must be an \
        object (non-object roots produce no output and exit 1). Missing paths \
        are silently omitted unless `--null-missing` is set, in which case \
        they appear as JSON `null`.\n\n\
        Output keys are sorted alphabetically (the sak deterministic-output \
        convention) regardless of the order they appear in the spec. To force \
        a specific ordering, alias each path with a key whose alphabetical \
        order matches the desired layout.\n\n\
        Composes well with `query`: run `query` first to land on the subtree \
        of interest, then pipe into `select` to extract a flat record. With \
        `--lines`, the input is parsed as NDJSON (one JSON value per line) \
        and the projection is applied to each record, emitting one object \
        per line in input order.",
    after_help = "\
Examples:
  echo '{\"name\":\"alice\",\"age\":30,\"city\":\"nyc\"}' | sak json select .name,.age
  sak json select .name,.age data.json
  sak json select 'id=.user.id,name=.user.name' data.json   Alias nested paths
  sak json select --null-missing .a,.b data.json            Missing paths -> null
  sak json select --pretty .name,.age data.json             Pretty-print output
  sak json select --lines .level,.msg events.ndjson         NDJSON: one object per line"
)]
pub struct SelectArgs {
    /// Comma-separated list of paths to project (each `path` or `alias=path`)
    pub paths: String,

    /// Input files (reads stdin if omitted)
    pub files: Vec<PathBuf>,

    /// Include missing paths as JSON null instead of omitting them
    #[arg(long)]
    pub null_missing: bool,

    /// Compact output (default)
    #[arg(long, conflicts_with = "pretty")]
    pub compact: bool,

    /// Pretty-print output
    #[arg(long)]
    pub pretty: bool,

    /// Parse input as NDJSON (one JSON value per line)
    #[arg(long)]
    pub lines: bool,

    /// Maximum number of output lines
    #[arg(long)]
    pub limit: Option<usize>,
}

#[derive(Debug)]
struct Field {
    alias: String,
    path: String,
}

/// Parse the comma-separated `paths` argument into ordered fields.
fn parse_fields(spec: &str) -> Result<Vec<Field>> {
    let mut out: Vec<Field> = Vec::new();
    for raw in spec.split(',') {
        let part = raw.trim();
        if part.is_empty() {
            bail!("empty path in --paths spec");
        }
        let (alias, path) = if let Some((alias, path)) = part.split_once('=') {
            let alias = alias.trim();
            let path = path.trim();
            if alias.is_empty() {
                bail!("empty alias in '{}'", part);
            }
            if path.is_empty() {
                bail!("empty path after '=' in '{}'", part);
            }
            (alias.to_string(), path.to_string())
        } else {
            let alias = default_alias(part)?;
            (alias, part.to_string())
        };
        if out.iter().any(|f| f.alias == alias) {
            bail!(
                "duplicate output key '{}' — disambiguate with '<alias>=<path>'",
                alias
            );
        }
        out.push(Field { alias, path });
    }
    if out.is_empty() {
        bail!("at least one path is required");
    }
    Ok(out)
}

/// Derive a default output key from a path expression.
///
/// For dot notation, the last `.key` segment wins. For JSON Pointer, the last
/// `/segment` wins (with `~1` decoded back to `/` and `~0` to `~` per RFC 6901).
/// Returns an error for paths that have no usable trailing key (e.g. the root
/// path `.`, a bare index `.users[0]`, or the empty pointer `""`).
fn default_alias(path: &str) -> Result<String> {
    if let Some(rest) = path.strip_prefix('/') {
        let last = rest.rsplit('/').next().unwrap_or("");
        if last.is_empty() {
            bail!(
                "cannot derive output key from '{}' — use '<alias>=<path>'",
                path
            );
        }
        return Ok(unescape_pointer_token(last));
    }
    // A path ending in `]` resolves to an array element with no natural name —
    // require the caller to pick one explicitly.
    if path.ends_with(']') {
        bail!(
            "cannot derive output key from '{}' — use '<alias>=<path>'",
            path
        );
    }
    let trimmed = path.trim_start_matches('.');
    let last = trimmed.rsplit('.').find(|seg| !seg.is_empty()).unwrap_or("");
    if last.is_empty() {
        bail!(
            "cannot derive output key from '{}' — use '<alias>=<path>'",
            path
        );
    }
    Ok(last.to_string())
}

fn unescape_pointer_token(s: &str) -> String {
    s.replace("~1", "/").replace("~0", "~")
}

pub fn run(args: &SelectArgs) -> Result<ExitCode> {
    let fields = parse_fields(&args.paths)?;
    let inputs = read_json_inputs_maybe_lines(&args.files, args.lines)?;

    let stdout = io::stdout();
    let handle = stdout.lock();
    let mut writer = BoundedWriter::new(handle, args.limit);

    let mut found_any = false;
    for (_name, value) in &inputs {
        if !value.is_object() {
            continue;
        }
        let projected = project(value, &fields, args.null_missing)?;
        if projected.is_empty() && !args.null_missing {
            continue;
        }
        found_any = true;
        let out = Value::Object(projected);
        let formatted = if args.pretty {
            serde_json::to_string_pretty(&out).unwrap_or_default()
        } else {
            serde_json::to_string(&out).unwrap_or_default()
        };
        for line in formatted.split('\n') {
            if !writer.write_line(line)? {
                writer.flush()?;
                return Ok(ExitCode::SUCCESS);
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

fn project(value: &Value, fields: &[Field], null_missing: bool) -> Result<Map<String, Value>> {
    let mut out = Map::new();
    for f in fields {
        match resolve_expression(value, &f.path)? {
            Some(v) => {
                out.insert(f.alias.clone(), v.clone());
            }
            None if null_missing => {
                out.insert(f.alias.clone(), Value::Null);
            }
            None => {}
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::io::Write;

    fn write_tmp(content: &str) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.json");
        let mut f = std::fs::File::create(&p).unwrap();
        f.write_all(content.as_bytes()).unwrap();
        (dir, p)
    }

    #[test]
    fn default_alias_dot_path() {
        assert_eq!(default_alias(".name").unwrap(), "name");
        assert_eq!(default_alias("name").unwrap(), "name");
        assert_eq!(default_alias(".user.id").unwrap(), "id");
        assert_eq!(default_alias(".users[0].name").unwrap(), "name");
    }

    #[test]
    fn default_alias_pointer() {
        assert_eq!(default_alias("/user/name").unwrap(), "name");
        // ~1 is the JSON Pointer escape for '/'
        assert_eq!(default_alias("/data/tls~1crt").unwrap(), "tls/crt");
    }

    #[test]
    fn default_alias_rejects_root_and_bare_index() {
        assert!(default_alias(".").is_err());
        assert!(default_alias("").is_err());
        assert!(default_alias(".users[0]").is_err());
        assert!(default_alias("/").is_err());
    }

    #[test]
    fn parse_fields_with_alias_and_default() {
        let fields = parse_fields("id=.user.id, .name").unwrap();
        assert_eq!(fields.len(), 2);
        assert_eq!(fields[0].alias, "id");
        assert_eq!(fields[0].path, ".user.id");
        assert_eq!(fields[1].alias, "name");
        assert_eq!(fields[1].path, ".name");
    }

    #[test]
    fn parse_fields_rejects_duplicate_alias() {
        let err = parse_fields(".user.name, .name").unwrap_err().to_string();
        assert!(err.contains("duplicate"), "{err}");
    }

    #[test]
    fn parse_fields_rejects_empty() {
        assert!(parse_fields("").is_err());
        assert!(parse_fields(".name,").is_err());
        assert!(parse_fields("=.foo").is_err());
        assert!(parse_fields("alias=").is_err());
    }

    #[test]
    fn project_basic() {
        let v = json!({"name": "alice", "age": 30, "city": "nyc"});
        let fields = parse_fields(".name,.age").unwrap();
        let m = project(&v, &fields, false).unwrap();
        assert_eq!(m.len(), 2);
        assert_eq!(m.get("name"), Some(&json!("alice")));
        assert_eq!(m.get("age"), Some(&json!(30)));
        assert!(!m.contains_key("city"));
    }

    #[test]
    fn project_nested_with_alias() {
        let v = json!({"user": {"id": 7, "name": "alice"}, "ts": "2024"});
        let fields = parse_fields("id=.user.id,name=.user.name,ts=.ts").unwrap();
        let m = project(&v, &fields, false).unwrap();
        assert_eq!(m.get("id"), Some(&json!(7)));
        assert_eq!(m.get("name"), Some(&json!("alice")));
        assert_eq!(m.get("ts"), Some(&json!("2024")));
    }

    #[test]
    fn project_missing_omitted_by_default() {
        let v = json!({"a": 1});
        let fields = parse_fields(".a,.b").unwrap();
        let m = project(&v, &fields, false).unwrap();
        assert_eq!(m.len(), 1);
        assert_eq!(m.get("a"), Some(&json!(1)));
    }

    #[test]
    fn project_missing_becomes_null_with_flag() {
        let v = json!({"a": 1});
        let fields = parse_fields(".a,.b").unwrap();
        let m = project(&v, &fields, true).unwrap();
        assert_eq!(m.len(), 2);
        assert_eq!(m.get("b"), Some(&Value::Null));
    }

    #[test]
    fn project_output_keys_are_sorted_alphabetically() {
        // sak convention: object output is sorted by key for deterministic
        // results, regardless of the projection order in the spec.
        let v = json!({"a": 1, "b": 2, "c": 3});
        let fields = parse_fields(".c,.a,.b").unwrap();
        let m = project(&v, &fields, false).unwrap();
        let keys: Vec<&String> = m.keys().collect();
        assert_eq!(keys, vec!["a", "b", "c"]);
    }

    #[test]
    fn project_supports_json_pointer() {
        let v = json!({"user": {"name": "alice"}});
        let fields = parse_fields("/user/name").unwrap();
        let m = project(&v, &fields, false).unwrap();
        assert_eq!(m.get("name"), Some(&json!("alice")));
    }

    #[test]
    fn run_simple() {
        let (_d, p) = write_tmp(r#"{"name":"alice","age":30}"#);
        let args = SelectArgs {
            paths: ".name,.age".to_string(),
            files: vec![p],
            null_missing: false,
            compact: false,
            pretty: false,
            lines: false,
            limit: None,
        };
        assert_eq!(run(&args).unwrap(), ExitCode::SUCCESS);
    }

    #[test]
    fn run_non_object_returns_1() {
        let (_d, p) = write_tmp("[1,2,3]");
        let args = SelectArgs {
            paths: ".name".to_string(),
            files: vec![p],
            null_missing: false,
            compact: false,
            pretty: false,
            lines: false,
            limit: None,
        };
        assert_eq!(run(&args).unwrap(), ExitCode::from(1));
    }

    #[test]
    fn run_all_paths_missing_returns_1() {
        let (_d, p) = write_tmp(r#"{"a":1}"#);
        let args = SelectArgs {
            paths: ".missing,.nope".to_string(),
            files: vec![p],
            null_missing: false,
            compact: false,
            pretty: false,
            lines: false,
            limit: None,
        };
        // No matches and not asking for nulls → no record emitted → exit 1.
        assert_eq!(run(&args).unwrap(), ExitCode::from(1));
    }

    #[test]
    fn run_all_paths_missing_with_null_succeeds() {
        let (_d, p) = write_tmp(r#"{"a":1}"#);
        let args = SelectArgs {
            paths: ".missing".to_string(),
            files: vec![p],
            null_missing: true,
            compact: false,
            pretty: false,
            lines: false,
            limit: None,
        };
        // null_missing always emits the record, so exit 0.
        assert_eq!(run(&args).unwrap(), ExitCode::SUCCESS);
    }

    #[test]
    fn run_lines_mode() {
        let (_d, p) = write_tmp("{\"name\":\"alice\",\"x\":1}\n{\"name\":\"bob\",\"x\":2}\n");
        let args = SelectArgs {
            paths: ".name".to_string(),
            files: vec![p],
            null_missing: false,
            compact: false,
            pretty: false,
            lines: true,
            limit: None,
        };
        assert_eq!(run(&args).unwrap(), ExitCode::SUCCESS);
    }
}
