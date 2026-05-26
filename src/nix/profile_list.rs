use std::process::ExitCode;

use anyhow::{Context, Result};
use clap::Args;
use serde_json::Value;

use crate::nix::Format;
use crate::nix::client;
use crate::output::BoundedWriter;

#[derive(Args)]
#[command(
    about = "List installed Nix profile packages as TSV (read-only)",
    long_about = "List the packages installed in a Nix profile via `nix profile list --json` \
        and emit one TSV row per element with the columns index, name, store-path, \
        flake-attr.\n\n\
        The default profile is the caller's `~/.nix-profile`; pass `--profile \
        <path>` to inspect another. `name` is the element's profile name (newer \
        nix keys elements by name; on older nix that has no names, it renders \
        `-`). `store-path` is the element's realised output path(s) — joined with \
        a comma if an element produced several. `flake-attr` is the reinstallable \
        reference `<originalUrl>#<attrPath>` (e.g. `flake:nixpkgs#legacyPackages.\
        x86_64-linux.hello`).\n\n\
        `--format json` passes nix's full element object through (it carries more \
        than the four TSV columns — `active`, `priority`, `url`, ...).\n\n\
        Exit status: 0 when at least one package is listed, 1 when the profile is \
        empty, 2 on error.",
    after_help = "\
Examples:
  sak nix profile-list                         Packages in ~/.nix-profile as TSV
  sak nix profile-list --profile /nix/var/nix/profiles/system
  sak nix profile-list --format json           Full nix element objects"
)]
pub struct ProfileListArgs {
    /// Explicit profile location (default: the caller's `~/.nix-profile`)
    #[arg(long, value_name = "PATH")]
    pub profile: Option<String>,

    /// Output format
    #[arg(long, value_enum, default_value_t = Format::Tsv)]
    pub format: Format,

    /// Maximum number of output lines (bounds output, not the nix call)
    #[arg(long)]
    pub limit: Option<usize>,
}

/// Fixed TSV column set, in emission order.
const COLUMNS: [&str; 4] = ["index", "name", "store-path", "flake-attr"];

/// One profile element row.
#[derive(Debug, PartialEq, Eq)]
pub struct Row {
    pub index: usize,
    pub name: String,
    pub store_path: String,
    pub flake_attr: String,
}

impl Row {
    fn to_tsv(&self) -> String {
        format!(
            "{}\t{}\t{}\t{}",
            self.index, self.name, self.store_path, self.flake_attr
        )
    }
}

pub fn run(args: &ProfileListArgs) -> Result<ExitCode> {
    let mut argv = vec!["--json"];
    if let Some(profile) = &args.profile {
        argv.push("--profile");
        argv.push(profile);
    }
    let stdout = client::invoke_ok("profile", Some("list"), &argv)?;

    let text = String::from_utf8_lossy(&stdout);
    let value: Value =
        serde_json::from_str(text.trim()).context("parsing `nix profile list --json` output")?;
    let rows = walk(&value);

    // Empty profile is "no results" for both formats — consistent exit 1.
    if rows.is_empty() {
        return Ok(ExitCode::from(1));
    }

    let out = std::io::stdout();
    let mut writer = BoundedWriter::new(out.lock(), args.limit);
    match args.format {
        Format::Json => {
            // Pass nix's full payload through (richer than the TSV columns).
            for line in text.split_inclusive('\n') {
                let line = line.strip_suffix('\n').unwrap_or(line);
                if !writer.write_line(line)? {
                    break;
                }
            }
        }
        Format::Tsv => {
            writer.write_decoration(&COLUMNS.join("\t"))?;
            for row in &rows {
                if !writer.write_line(&row.to_tsv())? {
                    break;
                }
            }
        }
    }
    writer.flush()?;
    Ok(ExitCode::SUCCESS)
}

/// Project `nix profile list --json` into rows. Handles both schema shapes:
/// newer nix keys `elements` by name (an object), older nix uses a positional
/// array (no names → `-`). Elements are ordered by name (object) or position
/// (array) for deterministic `index` values. Pure over its input so it's
/// testable on hand-built fixtures.
pub fn walk(value: &Value) -> Vec<Row> {
    match value.get("elements") {
        Some(Value::Object(map)) => {
            let mut entries: Vec<(&String, &Value)> = map.iter().collect();
            entries.sort_by(|a, b| a.0.cmp(b.0));
            entries
                .into_iter()
                .enumerate()
                .map(|(i, (name, el))| row_from(i, Some(name), el))
                .collect()
        }
        Some(Value::Array(arr)) => arr
            .iter()
            .enumerate()
            .map(|(i, el)| row_from(i, None, el))
            .collect(),
        _ => Vec::new(),
    }
}

fn row_from(index: usize, name: Option<&str>, el: &Value) -> Row {
    Row {
        index,
        name: name.unwrap_or("-").to_string(),
        store_path: store_paths(el),
        flake_attr: flake_attr(el),
    }
}

/// Join an element's realised output paths with a comma; `-` if none.
fn store_paths(el: &Value) -> String {
    match el.get("storePaths").and_then(Value::as_array) {
        Some(arr) if !arr.is_empty() => arr
            .iter()
            .filter_map(Value::as_str)
            .collect::<Vec<_>>()
            .join(","),
        _ => "-".to_string(),
    }
}

/// The reinstallable reference `<originalUrl>#<attrPath>`; falls back to
/// whichever part is present, or `-` if neither is.
fn flake_attr(el: &Value) -> String {
    let orig = el.get("originalUrl").and_then(Value::as_str);
    let attr = el.get("attrPath").and_then(Value::as_str);
    match (orig, attr) {
        (Some(o), Some(a)) => format!("{o}#{a}"),
        (Some(o), None) => o.to_string(),
        (None, Some(a)) => a.to_string(),
        (None, None) => "-".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn walk_v3_object_elements_sorted_by_name() {
        let v = json!({
            "version": 3,
            "elements": {
                "ripgrep": {
                    "attrPath": "legacyPackages.x86_64-linux.ripgrep",
                    "originalUrl": "flake:nixpkgs",
                    "storePaths": ["/nix/store/aaa-ripgrep-14"]
                },
                "hello": {
                    "attrPath": "legacyPackages.x86_64-linux.hello",
                    "originalUrl": "flake:nixpkgs",
                    "storePaths": ["/nix/store/zzz-hello-2.12.3"]
                }
            }
        });
        let rows = walk(&v);
        assert_eq!(rows.len(), 2);
        // Sorted by name: hello (0) before ripgrep (1).
        assert_eq!(rows[0].index, 0);
        assert_eq!(rows[0].name, "hello");
        assert_eq!(rows[0].store_path, "/nix/store/zzz-hello-2.12.3");
        assert_eq!(
            rows[0].flake_attr,
            "flake:nixpkgs#legacyPackages.x86_64-linux.hello"
        );
        assert_eq!(rows[1].index, 1);
        assert_eq!(rows[1].name, "ripgrep");
    }

    #[test]
    fn walk_v2_array_elements_use_position_and_dash_name() {
        let v = json!({
            "version": 2,
            "elements": [
                { "attrPath": "packages.x86_64-linux.foo", "originalUrl": "flake:foo",
                  "storePaths": ["/nix/store/p1", "/nix/store/p2"] }
            ]
        });
        let rows = walk(&v);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].index, 0);
        assert_eq!(rows[0].name, "-"); // array form has no name
        assert_eq!(rows[0].store_path, "/nix/store/p1,/nix/store/p2"); // joined
        assert_eq!(rows[0].flake_attr, "flake:foo#packages.x86_64-linux.foo");
    }

    #[test]
    fn walk_empty_profile_yields_no_rows() {
        assert!(walk(&json!({"elements": {}, "version": 3})).is_empty());
        assert!(walk(&json!({"elements": [], "version": 2})).is_empty());
    }

    #[test]
    fn flake_attr_falls_back_to_available_part() {
        assert_eq!(flake_attr(&json!({"attrPath": "a.b"})), "a.b");
        assert_eq!(flake_attr(&json!({"originalUrl": "flake:x"})), "flake:x");
        assert_eq!(flake_attr(&json!({})), "-");
    }

    #[test]
    fn store_paths_absent_renders_dash() {
        assert_eq!(store_paths(&json!({})), "-");
        assert_eq!(store_paths(&json!({"storePaths": []})), "-");
    }

    #[test]
    fn row_to_tsv_is_tab_separated() {
        let row = Row {
            index: 0,
            name: "hello".to_string(),
            store_path: "/nix/store/x".to_string(),
            flake_attr: "flake:nixpkgs#hello".to_string(),
        };
        assert_eq!(row.to_tsv(), "0\thello\t/nix/store/x\tflake:nixpkgs#hello");
    }
}
