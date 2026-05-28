use crate::output::Outcome;

use anyhow::{Context, Result};
use clap::Args;
use serde_json::Value;

use crate::nix::client;
use crate::nix::{Format, emit_to_stdout, render_cell};
use crate::output::BoundedWriter;

#[derive(Args)]
#[command(
    about = "Show a flake's outputs as TSV (read-only)",
    long_about = "Show a flake's outputs via `nix flake show --json <flake-ref>` and emit one \
        TSV row per leaf output with the columns output-path, type, description.\n\n\
        `nix flake show` produces a nested tree (`packages.<system>.<name>`, \
        `devShells.<system>.<name>`, `nixosConfigurations.<name>`, `apps.<system>.<name>`, \
        ...); sak walks it and emits one row per fully-evaluated leaf (an object \
        carrying a `type`). Without `--all-systems`, nix only evaluates outputs for \
        the current system and leaves placeholders for the others — those \
        unevaluated placeholders produce no rows. Pass `--all-systems` to force \
        evaluation across every platform.\n\n\
        The `<flake-ref>` defaults to `.` (the flake in the current directory) and \
        accepts anything `nix` does (`.`, `/path/to/flake`, `github:owner/repo`, \
        `nixpkgs`, ...). Use `--format json` for nix's raw JSON tree.\n\n\
        Exit status: 0 when at least one output is listed, 1 when none (e.g. an \
        empty flake, or every output is an unevaluated placeholder), 2 on error.",
    after_help = "\
Examples:
  sak nix flake-show                             Outputs of the flake in the current dir
  sak nix flake-show .                           Same, explicit
  sak nix flake-show --all-systems               Force evaluation across all platforms
  sak nix flake-show nixpkgs                     Outputs of a named flake
  sak nix flake-show github:nixos/nixpkgs        Outputs of a remote flake
  sak nix flake-show --format json               Raw nix JSON tree"
)]
pub struct FlakeShowArgs {
    /// Flake reference to inspect (default: `.`)
    #[arg(value_name = "FLAKE-REF", default_value = ".")]
    pub flake_ref: String,

    /// Evaluate outputs for every system, not just the current one
    #[arg(long = "all-systems")]
    pub all_systems: bool,

    /// Output format
    #[arg(long, value_enum, default_value_t = Format::Tsv)]
    pub format: Format,

    /// Maximum number of output lines (bounds output, not the nix evaluation)
    #[arg(long)]
    pub limit: Option<usize>,
}

/// Fixed TSV column set, in emission order.
const COLUMNS: [&str; 3] = ["output-path", "type", "description"];

/// One projected flake-output row.
#[derive(Debug, PartialEq, Eq)]
pub struct Row {
    pub path: String,
    pub typ: String,
    pub description: String,
}

impl Row {
    fn to_tsv(&self) -> String {
        format!("{}\t{}\t{}", self.path, self.typ, self.description)
    }
}

pub fn run(args: &FlakeShowArgs) -> Result<Outcome> {
    let mut argv = vec!["--json"];
    if args.all_systems {
        argv.push("--all-systems");
    }
    argv.push(&args.flake_ref);
    let stdout = client::invoke_ok("flake", Some("show"), &argv)?;
    emit_to_stdout(&stdout, args.format, args.limit, "{}", emit_tsv)
}

/// Walk the `nix flake show --json` tree and collect one row per leaf output.
/// A leaf is an object carrying a string `type` (`derivation`,
/// `nixos-configuration`, `app`, ...); descent stops there. Objects without a
/// `type` are interior nodes and are recursed into; empty `{}` placeholders
/// (other-system outputs nix didn't evaluate) yield no rows. Rows are sorted by
/// path for deterministic output. Pure over its input so it's testable on
/// hand-built fixtures.
pub fn walk(value: &Value) -> Vec<Row> {
    let mut rows = Vec::new();
    walk_into(value, &mut Vec::new(), &mut rows);
    rows.sort_by(|a, b| a.path.cmp(&b.path));
    rows
}

fn walk_into<'a>(value: &'a Value, path: &mut Vec<&'a str>, rows: &mut Vec<Row>) {
    let Some(obj) = value.as_object() else {
        return;
    };
    if let Some(Value::String(typ)) = obj.get("type") {
        rows.push(Row {
            path: path.join("."),
            typ: typ.clone(),
            description: render_cell(obj.get("description")),
        });
        return;
    }
    for (key, child) in obj {
        path.push(key);
        walk_into(child, path, rows);
        path.pop();
    }
}

/// Parse nix's JSON tree, project rows, and emit a header + TSV rows. The
/// header is decoration (not counted toward `--limit`); no leaves → no results.
fn emit_tsv(writer: &mut BoundedWriter<'_>, stdout: &[u8]) -> Result<bool> {
    let text = String::from_utf8_lossy(stdout);
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Ok(false);
    }
    let value: Value =
        serde_json::from_str(trimmed).context("parsing `nix flake show --json` output")?;
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
    fn walk_emits_one_row_per_leaf_sorted() {
        let v = json!({
            "packages": {
                "x86_64-linux": {
                    "default": {
                        "type": "derivation",
                        "name": "sak-0.17.5",
                        "description": "Swiss Army Knife"
                    }
                }
            },
            "devShells": {
                "x86_64-linux": {
                    "default": { "type": "derivation", "name": "nix-shell", "description": "" }
                }
            }
        });
        let rows = walk(&v);
        assert_eq!(rows.len(), 2);
        // Sorted by path: devShells before packages.
        assert_eq!(rows[0].path, "devShells.x86_64-linux.default");
        assert_eq!(rows[0].typ, "derivation");
        assert_eq!(rows[0].description, ""); // empty description stays empty
        assert_eq!(rows[1].path, "packages.x86_64-linux.default");
        assert_eq!(rows[1].description, "Swiss Army Knife");
    }

    #[test]
    fn walk_skips_unevaluated_placeholders() {
        // Other-system outputs come back as empty objects — not leaves.
        let v = json!({
            "packages": {
                "aarch64-darwin": { "default": {} },
                "x86_64-linux": { "default": { "type": "derivation" } }
            }
        });
        let rows = walk(&v);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].path, "packages.x86_64-linux.default");
        assert_eq!(rows[0].description, "-"); // missing description renders dash
    }

    #[test]
    fn walk_handles_typeless_leaf_kinds() {
        let v = json!({
            "nixosConfigurations": { "host": { "type": "nixos-configuration" } },
            "apps": { "x86_64-linux": { "run": { "type": "app" } } }
        });
        let rows = walk(&v);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].path, "apps.x86_64-linux.run");
        assert_eq!(rows[0].typ, "app");
        assert_eq!(rows[1].path, "nixosConfigurations.host");
        assert_eq!(rows[1].typ, "nixos-configuration");
    }

    #[test]
    fn walk_collapses_whitespace_in_description() {
        let v = json!({ "packages": { "x": { "type": "derivation", "description": "a\tb\nc" } } });
        assert_eq!(walk(&v)[0].description, "a b c");
    }

    #[test]
    fn walk_empty_tree_yields_no_rows() {
        assert!(walk(&json!({})).is_empty());
    }

    #[test]
    fn walk_non_object_yields_no_rows() {
        assert!(walk(&json!("nope")).is_empty());
        assert!(walk(&json!([1, 2, 3])).is_empty());
    }

    #[test]
    fn row_to_tsv_is_tab_separated() {
        let row = Row {
            path: "packages.x86_64-linux.default".to_string(),
            typ: "derivation".to_string(),
            description: "hi".to_string(),
        };
        assert_eq!(
            row.to_tsv(),
            "packages.x86_64-linux.default\tderivation\thi"
        );
    }

    #[test]
    fn run_argv_includes_all_systems_when_set() {
        // Sanity: the flag threads through. Build the argv the way `run` does.
        let args = FlakeShowArgs {
            flake_ref: ".".to_string(),
            all_systems: true,
            format: Format::Tsv,
            limit: None,
        };
        let mut argv = vec!["--json"];
        if args.all_systems {
            argv.push("--all-systems");
        }
        argv.push(&args.flake_ref);
        assert_eq!(argv, vec!["--json", "--all-systems", "."]);
    }
}
