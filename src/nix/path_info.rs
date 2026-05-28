use crate::output::Outcome;

use anyhow::{Context, Result};
use clap::Args;
use serde_json::Value;

use crate::nix::client;
use crate::nix::{Format, emit_to_stdout, render_cell};
use crate::output::BoundedWriter;

#[derive(Args)]
#[command(
    about = "Show store path metadata as TSV (read-only)",
    long_about = "Show metadata for one or more store paths via `nix path-info --json` and emit \
        one TSV row per path with the columns path, nar_size, closure_size, \
        deriver, signatures.\n\n\
        `nar_size` is the path's own NAR size in bytes. `closure_size` is the \
        total size of the path's transitive closure in bytes — it is only \
        computed when you pass `--closure` (otherwise it renders `-`, since the \
        recursive walk is not free). `deriver` is the `.drv` that built the path \
        (or `-`); `signatures` is the binary-cache signatures joined with `,` (or \
        `-`).\n\n\
        Accepts multiple store paths (or symlinks into the store). `--format \
        json` passes nix's full per-path object through (it also carries \
        references, narHash, registrationTime, ...).\n\n\
        Exit status: 0 when at least one path is described, 1 when none, 2 on \
        error (e.g. a path not in the store).",
    after_help = "\
Examples:
  sak nix path-info /nix/store/…-hello          NAR size, deriver, signatures
  sak nix path-info --closure /nix/store/…-hello   Include the closure size
  sak nix path-info /nix/store/a /nix/store/b   Several paths at once
  sak nix path-info --format json /nix/store/…-hello"
)]
pub struct PathInfoArgs {
    /// Store path(s) (or symlinks into the store) to describe
    #[arg(value_name = "PATH", required = true)]
    pub paths: Vec<String>,

    /// Also compute each path's transitive closure size (passes `--closure-size`)
    #[arg(long)]
    pub closure: bool,

    /// Output format
    #[arg(long, value_enum, default_value_t = Format::Tsv)]
    pub format: Format,

    /// Maximum number of output lines (bounds output, not the nix call)
    #[arg(long)]
    pub limit: Option<usize>,
}

/// Fixed TSV column set, in emission order.
const COLUMNS: [&str; 5] = ["path", "nar_size", "closure_size", "deriver", "signatures"];

/// One projected path-info row.
#[derive(Debug, PartialEq, Eq)]
pub struct Row {
    pub path: String,
    pub nar_size: String,
    pub closure_size: String,
    pub deriver: String,
    pub signatures: String,
}

impl Row {
    fn to_tsv(&self) -> String {
        format!(
            "{}\t{}\t{}\t{}\t{}",
            self.path, self.nar_size, self.closure_size, self.deriver, self.signatures
        )
    }
}

pub fn run(args: &PathInfoArgs) -> Result<Outcome> {
    let mut argv = vec!["--json".to_string()];
    if args.closure {
        argv.push("--closure-size".to_string());
    }
    argv.extend(args.paths.iter().cloned());
    let argv_refs: Vec<&str> = argv.iter().map(String::as_str).collect();
    let stdout = client::invoke_ok("path-info", None, &argv_refs)?;
    emit_to_stdout(&stdout, args.format, args.limit, "{}", emit_tsv)
}

/// Project `nix path-info --json` into rows. Modern nix returns an object keyed
/// by store path; older nix returned an array of objects each carrying a `path`
/// field — both are handled. Object entries are sorted by path for
/// deterministic output. Pure over its input so it's testable on hand-built
/// fixtures.
pub fn walk(value: &Value) -> Vec<Row> {
    match value {
        Value::Object(map) => {
            let mut entries: Vec<(&String, &Value)> = map.iter().collect();
            entries.sort_by(|a, b| a.0.cmp(b.0));
            entries
                .into_iter()
                .map(|(path, el)| row_from(path, el))
                .collect()
        }
        Value::Array(arr) => arr
            .iter()
            .filter_map(|el| {
                let path = el.get("path").and_then(Value::as_str)?;
                Some(row_from(path, el))
            })
            .collect(),
        _ => Vec::new(),
    }
}

fn row_from(path: &str, el: &Value) -> Row {
    Row {
        path: path.to_string(),
        nar_size: render_cell(el.get("narSize")),
        closure_size: render_cell(el.get("closureSize")),
        deriver: render_cell(el.get("deriver")),
        signatures: signatures(el),
    }
}

/// Join a path's binary-cache signatures with `,`; `-` if none. Modern nix uses
/// the `signatures` key; older nix used `sigs`.
fn signatures(el: &Value) -> String {
    let arr = el
        .get("signatures")
        .or_else(|| el.get("sigs"))
        .and_then(Value::as_array);
    match arr {
        Some(a) if !a.is_empty() => a
            .iter()
            .filter_map(Value::as_str)
            .collect::<Vec<_>>()
            .join(","),
        _ => "-".to_string(),
    }
}

/// Parse nix's path-info object, project rows, and emit a header + TSV rows. An
/// empty / `{}` body counts as "no results".
fn emit_tsv(writer: &mut BoundedWriter<'_>, stdout: &[u8]) -> Result<bool> {
    let text = String::from_utf8_lossy(stdout);
    let trimmed = text.trim();
    if trimmed.is_empty() || trimmed == "{}" {
        return Ok(false);
    }
    let value: Value =
        serde_json::from_str(trimmed).context("parsing `nix path-info --json` output")?;
    let rows = walk(&value);
    if rows.is_empty() {
        return Ok(false);
    }
    writer.write_decoration(&COLUMNS.join("\t"))?;
    for row in &rows {
        if !writer.write_line(&row.to_tsv())? {
            break;
        }
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn walk_object_keyed_by_path_sorted() {
        let v = json!({
            "/nix/store/bbb-foo": { "narSize": 200, "deriver": "/nix/store/d2.drv",
                "signatures": ["cache.nixos.org-1:sigB"] },
            "/nix/store/aaa-bar": { "narSize": 100, "closureSize": 5000, "deriver": null,
                "signatures": [] }
        });
        let rows = walk(&v);
        assert_eq!(rows.len(), 2);
        // Sorted by path: aaa before bbb.
        assert_eq!(rows[0].path, "/nix/store/aaa-bar");
        assert_eq!(rows[0].nar_size, "100");
        assert_eq!(rows[0].closure_size, "5000");
        assert_eq!(rows[0].deriver, "-"); // null
        assert_eq!(rows[0].signatures, "-"); // empty array
        assert_eq!(rows[1].path, "/nix/store/bbb-foo");
        assert_eq!(rows[1].closure_size, "-"); // closureSize absent without --closure
        assert_eq!(rows[1].signatures, "cache.nixos.org-1:sigB");
    }

    #[test]
    fn walk_array_form_uses_path_field() {
        let v = json!([
            { "path": "/nix/store/x", "narSize": 42, "sigs": ["s1", "s2"] }
        ]);
        let rows = walk(&v);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].path, "/nix/store/x");
        assert_eq!(rows[0].nar_size, "42");
        // Older `sigs` key, joined.
        assert_eq!(rows[0].signatures, "s1,s2");
    }

    #[test]
    fn walk_empty_and_non_collection_yield_no_rows() {
        assert!(walk(&json!({})).is_empty());
        assert!(walk(&json!([])).is_empty());
        assert!(walk(&json!("nope")).is_empty());
    }

    #[test]
    fn row_to_tsv_is_tab_separated() {
        let row = Row {
            path: "/nix/store/x".to_string(),
            nar_size: "100".to_string(),
            closure_size: "-".to_string(),
            deriver: "/nix/store/d.drv".to_string(),
            signatures: "s1".to_string(),
        };
        assert_eq!(row.to_tsv(), "/nix/store/x\t100\t-\t/nix/store/d.drv\ts1");
    }
}
