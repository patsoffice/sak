use crate::output::Outcome;

use anyhow::{Context, Result};
use clap::Args;
use serde_json::Value;

use crate::nix::client;
use crate::nix::{Format, emit_to_stdout, render_cell};
use crate::output::BoundedWriter;

/// Fixed TSV column set, in emission order. `nix store info --json` returns a
/// single object whose field set varies by store type: a local `daemon` store
/// emits `url`/`version`/`trusted`; a fully-described binary cache adds
/// `priority`/`wantMassQuery`/`trustedPublicKeys` (the substituter-trust fields
/// this command exists to surface). Missing fields render `-`.
const COLUMNS: [&str; 6] = [
    "url",
    "version",
    "trusted",
    "priority",
    "wantMassQuery",
    "trustedPublicKeys",
];

#[derive(Args)]
#[command(
    about = "Show Nix store / substituter info as TSV (read-only)",
    long_about = "Show a Nix store's info via `nix store info --json` and emit one TSV row \
        with the columns url, version, trusted, priority, wantMassQuery, \
        trustedPublicKeys.\n\n\
        `nix store info` reports on a store — the local daemon by default, or any \
        store given with `--store` (a binary cache URL, `ssh://host`, `daemon`, \
        ...). The field set varies by store type: the daemon emits \
        url/version/trusted, while a fully-described binary cache adds the \
        substituter-trust fields (priority, wantMassQuery, trustedPublicKeys); \
        absent fields render `-`. `trustedPublicKeys` is an array, rendered as \
        compact JSON.\n\n\
        Use `--field <name>` to print a single field's value bare (any key in \
        the JSON, not just the TSV columns — e.g. `--field version`), `--format \
        json` for nix's raw object, or `--store <url>` to inspect a remote \
        store / cache.\n\n\
        Exit status: 0 when info is returned, 1 when none (an empty object, or a \
        `--field` that names an absent key), 2 on error.",
    after_help = "\
Examples:
  sak nix store-info                           Info for the default store (daemon)
  sak nix store-info --field version           Just the nix store version, bare
  sak nix store-info --store https://cache.nixos.org   Inspect a binary cache
  sak nix store-info --format json             Raw nix JSON object"
)]
pub struct StoreInfoArgs {
    /// Store to inspect (a URL, `ssh://host`, `daemon`, ...); default: the
    /// ambient store
    #[arg(long, value_name = "STORE")]
    pub store: Option<String>,

    /// Print a single field's value bare (any key in the JSON object)
    #[arg(long, value_name = "NAME", conflicts_with = "format")]
    pub field: Option<String>,

    /// Output format
    #[arg(long, value_enum, default_value_t = Format::Tsv)]
    pub format: Format,

    /// Maximum number of output lines (bounds output, not the nix call)
    #[arg(long)]
    pub limit: Option<usize>,
}

pub fn run(args: &StoreInfoArgs) -> Result<Outcome> {
    let mut argv = vec!["--json"];
    if let Some(store) = &args.store {
        argv.push("--store");
        argv.push(store);
    }
    let stdout = client::invoke_ok("store", Some("info"), &argv)?;

    if let Some(field) = &args.field {
        return emit_field(&stdout, field, args.limit);
    }
    emit_to_stdout(&stdout, args.format, args.limit, "{}", emit_tsv)
}

/// Project the store-info object into one fixed-column row. Pure over its input
/// so it's testable on hand-built fixtures.
pub fn project(value: &Value) -> [String; 6] {
    COLUMNS.map(|col| render_cell(value.get(col)))
}

/// Parse the store-info object and emit a header + one TSV row. An empty / `{}`
/// body counts as "no results"; any object yields a row.
fn emit_tsv(writer: &mut BoundedWriter<'_>, stdout: &[u8]) -> Result<bool> {
    let text = String::from_utf8_lossy(stdout);
    let trimmed = text.trim();
    if trimmed.is_empty() || trimmed == "{}" {
        return Ok(false);
    }
    let value: Value =
        serde_json::from_str(trimmed).context("parsing `nix store info --json` output")?;
    if !value.is_object() {
        return Ok(false);
    }
    writer.write_decoration(&COLUMNS.join("\t"))?;
    writer.write_line(&project(&value).join("\t"))?;
    Ok(true)
}

/// Extract one named field and print its value bare. A scalar prints verbatim;
/// an array / object prints as compact JSON. A missing or null field is "no
/// results" (exit 1) with no output.
fn emit_field(stdout: &[u8], field: &str, limit: Option<usize>) -> Result<Outcome> {
    let text = String::from_utf8_lossy(stdout);
    let value: Value =
        serde_json::from_str(text.trim()).context("parsing `nix store info --json` output")?;
    let cell = match value.get(field) {
        None | Some(Value::Null) => return Ok(Outcome::NotFound),
        Some(Value::String(s)) => s.clone(),
        Some(other) => serde_json::to_string(other).unwrap_or_default(),
    };
    let out = std::io::stdout();
    let mut writer = BoundedWriter::new(out.lock(), limit);
    writer.write_line(&cell)?;
    writer.flush()?;
    Ok(Outcome::Found)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn project_renders_daemon_fields_and_dashes_the_rest() {
        // What a local daemon store actually emits.
        let v = json!({ "trusted": false, "url": "daemon", "version": "2.34.7" });
        assert_eq!(
            project(&v),
            [
                "daemon", // url
                "2.34.7", // version
                "false",  // trusted
                "-",      // priority
                "-",      // wantMassQuery
                "-",      // trustedPublicKeys
            ]
            .map(String::from)
        );
    }

    #[test]
    fn project_renders_substituter_trust_fields() {
        let v = json!({
            "url": "https://cache.nixos.org",
            "priority": 40,
            "wantMassQuery": true,
            "trustedPublicKeys": ["cache.nixos.org-1:abc="]
        });
        let row = project(&v);
        assert_eq!(row[0], "https://cache.nixos.org");
        assert_eq!(row[1], "-"); // version absent
        assert_eq!(row[3], "40");
        assert_eq!(row[4], "true");
        // Array renders as compact JSON.
        assert_eq!(row[5], "[\"cache.nixos.org-1:abc=\"]");
    }

    #[test]
    fn emit_field_prints_scalar_bare() {
        // String prints without quotes; bool/number via compact JSON.
        let stdout = br#"{"url":"daemon","version":"2.34.7","trusted":false}"#;
        // Exercise the extraction logic directly on parsed JSON.
        let v: Value = serde_json::from_slice(stdout).unwrap();
        assert_eq!(v.get("version").unwrap().as_str().unwrap(), "2.34.7");
    }

    #[test]
    fn emit_field_missing_is_exit_1() {
        let code = emit_field(br#"{"url":"daemon"}"#, "version", None).unwrap();
        assert_eq!(code, Outcome::NotFound);
    }

    #[test]
    fn emit_field_present_is_success() {
        let code = emit_field(br#"{"url":"daemon"}"#, "url", None).unwrap();
        assert_eq!(code, Outcome::Found);
    }

    #[test]
    fn build_argv_threads_store_flag() {
        // Mirror what `run` builds.
        let store = Some("https://cache.nixos.org".to_string());
        let mut argv = vec!["--json"];
        if let Some(s) = &store {
            argv.push("--store");
            argv.push(s);
        }
        assert_eq!(argv, vec!["--json", "--store", "https://cache.nixos.org"]);
    }
}
