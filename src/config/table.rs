use crate::output::Outcome;
use std::io;
use std::path::PathBuf;

use anyhow::{Result, bail};
use clap::Args;
use serde_json::Value;

use crate::config::{Format, read_config_inputs};
use crate::json::table::{parse_fields, project_row};
use crate::output::BoundedWriter;
use crate::value::{resolve_expression, type_name};

#[derive(Args)]
#[command(
    about = "Project an array of objects to a TSV table (one row per element)",
    long_about = "Iterate a config array of objects at a path and project N named \
        fields from each element into a tab-separated row. Inputs parse into \
        `serde_json::Value` first, so paths, `--fields` handling, the \
        `--missing` token, and cell rendering behave exactly as in `sak json \
        table`.\n\n\
        `<array-path>` is a dot path (`.packages`) or JSON Pointer \
        (`/packages`) pointing at the array; it defaults to `.` (the document \
        root must then be an array — natural for a TOML/YAML array of tables). \
        The root resolving to anything other than an array is an error.\n\n\
        Each `--fields` entry is itself a dot path or JSON Pointer resolved \
        against every element. A field missing from an element (or resolving to \
        null) renders as the `--missing` token (default `-`). Tabs and newlines \
        inside a projected value are collapsed to spaces so a value can't break \
        the one-row-per-element TSV contract.\n\n\
        Format is auto-detected from the file extension or set explicitly with \
        `--format` (required for stdin). Multiple input files are concatenated.\n\n\
        Exit codes follow sak convention: 0 = at least one row emitted, 1 = the \
        array was empty (no rows), 2 = error (bad path, non-array, parse error).",
    after_help = "\
Examples:
  sak config table .package --fields name,version -f toml Cargo.lock
  sak config table .items --fields id,user.name,ts data.yaml   Nested field paths
  sak config table .rows --fields a,b --header data.toml        Emit a header row
  sak config table .rows --fields a,b --missing NULL data.yaml  Custom missing token"
)]
pub struct TableArgs {
    /// Path to the array of objects (dot notation or JSON Pointer; default `.`)
    #[arg(default_value = ".")]
    pub array_path: String,

    /// Input files (reads stdin if omitted or given as "-")
    pub files: Vec<PathBuf>,

    /// Force a specific format (required for stdin)
    #[arg(short, long, value_enum)]
    pub format: Option<Format>,

    /// Comma-separated field paths to project from each element (required)
    /// (long only: `-f` is already `--format` in the config domain)
    #[arg(long, required = true)]
    pub fields: String,

    /// Emit a header row (the field paths, tab-separated)
    #[arg(long)]
    pub header: bool,

    /// Token substituted for missing/null fields
    #[arg(long, default_value = "-")]
    pub missing: String,

    /// Maximum number of output lines
    #[arg(long)]
    pub limit: Option<usize>,
}

pub fn run(args: &TableArgs) -> Result<Outcome> {
    let fields = parse_fields(&args.fields)?;
    // `sak config table --fields a,b Cargo.lock` puts the file in the
    // array-path slot; a lone existing file there is the input, path `.`.
    let (path, files) = crate::input::path_or_file(Some(&args.array_path), &args.files);
    let array_path = path.unwrap_or_else(|| ".".to_string());
    let inputs = read_config_inputs(&files, args.format)?;

    // Gather every row source up front (refs into `inputs`, which outlives this
    // loop) so an empty result can suppress the header, matching `k8s get
    // --columns`.
    let mut elements: Vec<&Value> = Vec::new();
    for (name, value) in &inputs {
        match resolve_expression(value, &array_path)? {
            Some(Value::Array(items)) => elements.extend(items.iter()),
            Some(other) => bail!(
                "expected an array at {:?} in {}, found {}",
                array_path,
                name,
                type_name(other)
            ),
            None => bail!("path {:?} did not resolve in {}", array_path, name),
        }
    }

    let stdout = io::stdout();
    let handle = stdout.lock();
    let mut writer = BoundedWriter::new(handle, args.limit);

    if elements.is_empty() {
        writer.flush()?;
        return Ok(Outcome::NotFound);
    }

    if args.header {
        writer.write_decoration(&fields.join("\t"))?;
    }

    let mut wrote_any = false;
    for element in elements {
        let row = project_row(element, &fields, &args.missing)?;
        if !writer.write_line(&row.join("\t"))? {
            break;
        }
        wrote_any = true;
    }
    writer.flush()?;
    Ok(if wrote_any {
        Outcome::Found
    } else {
        Outcome::NotFound
    })
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

    fn args_for(array_path: &str, fields: &str, p: PathBuf) -> TableArgs {
        TableArgs {
            array_path: array_path.to_string(),
            files: vec![p],
            format: None,
            fields: fields.to_string(),
            header: false,
            missing: "-".to_string(),
            limit: None,
        }
    }

    #[test]
    fn run_toml_wrapped_array() {
        let (_d, p) = write_tmp(
            "a.toml",
            "[[pkgs]]\nname = \"a\"\nversion = \"1\"\n[[pkgs]]\nname = \"b\"\nversion = \"2\"\n",
        );
        assert_eq!(
            run(&args_for(".pkgs", "name,version", p)).unwrap(),
            Outcome::Found
        );
    }

    #[test]
    fn run_yaml_root_array_default_path() {
        let (_d, p) = write_tmp("a.yaml", "- id: 1\n- id: 2\n");
        assert_eq!(run(&args_for(".", "id", p)).unwrap(), Outcome::Found);
    }

    #[test]
    fn run_empty_array_is_not_found() {
        let (_d, p) = write_tmp("a.toml", "items = []\n");
        let mut args = args_for(".items", "id", p);
        args.header = true;
        // Empty array → no rows, header suppressed, exit 1.
        assert_eq!(run(&args).unwrap(), Outcome::NotFound);
    }

    #[test]
    fn run_non_array_path_is_error() {
        let (_d, p) = write_tmp("a.yaml", "items:\n  not: an array\n");
        let err = run(&args_for(".items", "id", p)).unwrap_err().to_string();
        assert!(err.contains("expected an array"), "{err}");
    }

    #[test]
    fn run_missing_path_is_error() {
        let (_d, p) = write_tmp("a.toml", "items = []\n");
        let err = run(&args_for(".nope", "id", p)).unwrap_err().to_string();
        assert!(err.contains("did not resolve"), "{err}");
    }

    #[test]
    fn run_missing_field_uses_token() {
        let (_d, p) = write_tmp("a.toml", "[[pkgs]]\nname = \"a\"\n");
        let mut args = args_for(".pkgs", "name,version", p);
        args.missing = "NULL".to_string();
        assert_eq!(run(&args).unwrap(), Outcome::Found);
    }
}
