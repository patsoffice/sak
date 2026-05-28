use std::process::ExitCode;

use anyhow::{Context, Result};
use clap::Args;
use serde_json::Value;

use crate::nix::client;
use crate::nix::{Format, emit_to_stdout, render_cell};
use crate::output::BoundedWriter;

#[derive(Args)]
#[command(
    about = "Show a flake's metadata / lock freshness as TSV (read-only)",
    long_about = "Show a flake's metadata via `nix flake metadata --json` and emit one TSV row \
        with the columns locked.rev, locked.lastModified, locked.narHash, \
        original.url, path — the fields that answer \"is this lockfile fresh, and \
        what is it pinned to?\".\n\n\
        `locked.rev` is the resolved revision the flake is pinned to (a \
        working-tree / dirty source has no `rev`, only a dirty revision, so it \
        renders `-`). `path` is the store path the flake was copied to. \
        Any absent field renders `-`.\n\n\
        `<flake-ref>` defaults to `.` and accepts anything `nix` does (`.`, \
        `github:owner/repo`, `nixpkgs`, ...). Use `--field <name>` to print one \
        field bare — it takes a dotted path into nix's JSON (e.g. `--field \
        locked.rev`, `--field path`, `--field description`), not just the TSV \
        columns. `--format json` passes nix's full metadata object through (it \
        also carries `locks`, `fingerprint`, `resolved`, ...).\n\n\
        Exit status: 0 when metadata is returned, 1 when none (or a `--field` \
        that names an absent path), 2 on error.",
    after_help = "\
Examples:
  sak nix flake-metadata                        Metadata for the flake in the current dir
  sak nix flake-metadata --field locked.rev     Just the pinned revision, bare
  sak nix flake-metadata github:nixos/nixpkgs
  sak nix flake-metadata --format json          Raw nix metadata object"
)]
pub struct FlakeMetadataArgs {
    /// Flake reference to inspect (default: `.`)
    #[arg(value_name = "FLAKE-REF", default_value = ".")]
    pub flake_ref: String,

    /// Print a single field's value bare (a dotted path into nix's JSON)
    #[arg(long, value_name = "NAME", conflicts_with = "format")]
    pub field: Option<String>,

    /// Output format
    #[arg(long, value_enum, default_value_t = Format::Tsv)]
    pub format: Format,

    /// Maximum number of output lines (bounds output, not the nix call)
    #[arg(long)]
    pub limit: Option<usize>,
}

/// Fixed TSV column set, in emission order. Each is a dotted path into nix's
/// metadata object.
const COLUMNS: [&str; 5] = [
    "locked.rev",
    "locked.lastModified",
    "locked.narHash",
    "original.url",
    "path",
];

pub fn run(args: &FlakeMetadataArgs) -> Result<ExitCode> {
    let stdout = client::invoke_ok("flake", Some("metadata"), &["--json", &args.flake_ref])?;

    if let Some(field) = &args.field {
        return emit_field(&stdout, field, args.limit);
    }
    emit_to_stdout(&stdout, args.format, args.limit, "{}", emit_tsv)
}

/// Resolve a dotted path (`locked.rev`, `path`, ...) into nested objects.
/// Returns `None` if any segment is missing. Pure over its input.
pub fn get_path<'a>(value: &'a Value, path: &str) -> Option<&'a Value> {
    let mut cur = value;
    for seg in path.split('.') {
        cur = cur.get(seg)?;
    }
    Some(cur)
}

/// Project the metadata object into one fixed-column row. Pure over its input
/// so it's testable on hand-built fixtures.
pub fn project(value: &Value) -> [String; 5] {
    COLUMNS.map(|col| render_cell(get_path(value, col)))
}

/// Parse the metadata object and emit a header + one TSV row. An empty / `{}`
/// body counts as "no results".
fn emit_tsv(writer: &mut BoundedWriter<'_>, stdout: &[u8]) -> Result<bool> {
    let text = String::from_utf8_lossy(stdout);
    let trimmed = text.trim();
    if trimmed.is_empty() || trimmed == "{}" {
        return Ok(false);
    }
    let value: Value =
        serde_json::from_str(trimmed).context("parsing `nix flake metadata --json` output")?;
    if !value.is_object() {
        return Ok(false);
    }
    writer.write_decoration(&COLUMNS.join("\t"))?;
    writer.write_line(&project(&value).join("\t"))?;
    Ok(true)
}

/// Extract one dotted field and print its value bare. A scalar prints verbatim;
/// an array / object prints as compact JSON. A missing or null field is "no
/// results" (exit 1) with no output.
fn emit_field(stdout: &[u8], field: &str, limit: Option<usize>) -> Result<ExitCode> {
    let text = String::from_utf8_lossy(stdout);
    let value: Value =
        serde_json::from_str(text.trim()).context("parsing `nix flake metadata --json` output")?;
    let cell = match get_path(&value, field) {
        None | Some(Value::Null) => return Ok(ExitCode::from(1)),
        Some(Value::String(s)) => s.clone(),
        Some(other) => serde_json::to_string(other).unwrap_or_default(),
    };
    let out = std::io::stdout();
    let mut writer = BoundedWriter::new(out.lock(), limit);
    writer.write_line(&cell)?;
    writer.flush()?;
    Ok(ExitCode::SUCCESS)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn fixture() -> Value {
        json!({
            "description": "demo",
            "lastModified": 1779927902_i64,
            "locked": {
                "lastModified": 1779593580_i64,
                "narHash": "sha256-AAA=",
                "rev": "d849bb215dcdf71bce3e686839ccdb4219e84b2f",
                "type": "github"
            },
            "original": { "owner": "NixOS", "repo": "nixpkgs", "type": "github" },
            "path": "/nix/store/abc-source"
        })
    }

    #[test]
    fn project_pulls_nested_and_dashes_missing() {
        let row = project(&fixture());
        assert_eq!(row[0], "d849bb215dcdf71bce3e686839ccdb4219e84b2f"); // locked.rev
        assert_eq!(row[1], "1779593580"); // locked.lastModified
        assert_eq!(row[2], "sha256-AAA="); // locked.narHash
        assert_eq!(row[3], "-"); // original.url absent (github has owner/repo, no url)
        assert_eq!(row[4], "/nix/store/abc-source"); // path
    }

    #[test]
    fn project_dirty_source_has_no_rev() {
        // A dirty working tree exposes dirtyRev, not rev.
        let v = json!({ "locked": { "dirtyRev": "abc-dirty", "narHash": "sha256-X=" } });
        let row = project(&v);
        assert_eq!(row[0], "-"); // locked.rev absent
        assert_eq!(row[2], "sha256-X="); // narHash still present
    }

    #[test]
    fn get_path_walks_and_misses() {
        let v = fixture();
        assert_eq!(
            get_path(&v, "path").unwrap().as_str(),
            Some("/nix/store/abc-source")
        );
        assert_eq!(
            get_path(&v, "locked.type").unwrap().as_str(),
            Some("github")
        );
        assert!(get_path(&v, "locked.rev.nope").is_none());
        assert!(get_path(&v, "missing").is_none());
    }

    #[test]
    fn emit_field_present_and_missing() {
        let stdout = serde_json::to_vec(&fixture()).unwrap();
        let present = emit_field(&stdout, "locked.rev", None).unwrap();
        assert_eq!(format!("{present:?}"), format!("{:?}", ExitCode::SUCCESS));
        let missing = emit_field(&stdout, "locked.rev.nope", None).unwrap();
        assert_eq!(format!("{missing:?}"), format!("{:?}", ExitCode::from(1)));
    }
}
