use crate::output::Outcome;

use anyhow::{Context, Result};
use clap::Args;
use serde_json::Value;

use crate::helm::client::{self, Conn};
use crate::helm::{Format, emit_to_stdout, render_cell};
use crate::output::BoundedWriter;

/// Fixed TSV column set, in emission order. `helm status -o json` returns the
/// release object with `revision` under `version` and the rest under `info`,
/// so columns are (sometimes nested) lookups — see [`project`].
///
/// Notably absent: `chart` / `app_version`. `helm status -o json` does not
/// include the chart object (only `name`, `info`, `config`, `manifest`,
/// `version`, `namespace`), so there is nothing to project. Chart versions are
/// `sak helm list`'s job.
const COLUMNS: [&str; 6] = [
    "name",
    "namespace",
    "revision",
    "status",
    "last_deployed",
    "notes_present",
];

#[derive(Args)]
#[command(
    about = "Show one Helm release's status as TSV (read-only)",
    long_about = "Show a single Helm release's status via `helm status <release> -o json` \
        and emit one TSV row with the columns \
        name, namespace, revision, status, last_deployed, notes_present.\n\n\
        `notes_present` is a bool — the rendered NOTES.txt can be large, so the \
        TSV view reports only whether notes exist; use `--format json` to get \
        the full payload (info, manifest, config, and — with `--show-resources` \
        — the deployed resources). `--revision` inspects a specific past \
        revision.\n\n\
        Chart name / app version are not in `helm status` output; use `sak helm \
        list` for those.\n\n\
        Cluster, auth, and namespace resolution are whatever `helm` itself \
        uses (`KUBECONFIG` / `~/.kube/config`).\n\n\
        Exit status: 0 when the release is found, 1 when it does not exist, \
        2 on any other error.",
    after_help = "\
Examples:
  sak helm status cilium -n kube-system        Status of one release as TSV
  sak helm status cilium -n kube-system --format json
  sak helm status cilium -n kube-system --revision 16
  sak helm status cilium -n kube-system --show-resources --format json"
)]
pub struct StatusArgs {
    /// Release name to inspect
    #[arg(value_name = "RELEASE")]
    pub release: String,

    /// Namespace of the release (default: the kubeconfig's current namespace)
    #[arg(long, short = 'n', value_name = "NS")]
    pub namespace: Option<String>,

    /// Inspect a specific past revision instead of the latest
    #[arg(long, value_name = "N")]
    pub revision: Option<u32>,

    /// Include the release's deployed resources in `--format json` output
    #[arg(long)]
    pub show_resources: bool,

    /// Output format
    #[arg(long, value_enum, default_value_t = Format::Tsv)]
    pub format: Format,

    /// Maximum number of output lines (bounds output, not the helm fetch)
    #[arg(long)]
    pub limit: Option<usize>,
}

pub fn run(args: &StatusArgs) -> Result<Outcome> {
    let argv = build_argv(args);
    let argv_refs: Vec<&str> = argv.iter().map(String::as_str).collect();
    let conn = Conn {
        namespace: args.namespace.as_deref(),
        ..Default::default()
    };
    // A missing release is "no results" (exit 1), not a tool failure; the
    // chokepoint maps helm's not-found stderr to Ok(None).
    let Some(stdout) = client::invoke_found("status", None, &argv_refs, conn)? else {
        return Ok(Outcome::NotFound);
    };
    emit_to_stdout(&stdout, args.format, args.limit, "{}", emit_tsv)
}

/// Assemble the `helm status` argv (everything but the connection flags, which
/// the chokepoint applies from `Conn`). The release name is a positional
/// argument forwarded to `helm`.
fn build_argv(args: &StatusArgs) -> Vec<String> {
    let mut v = vec!["--output".to_string(), "json".to_string()];
    if let Some(rev) = args.revision {
        v.push("--revision".to_string());
        v.push(rev.to_string());
    }
    if args.show_resources {
        v.push("--show-resources".to_string());
    }
    v.push(args.release.clone());
    v
}

/// Project `helm status -o json`'s release object into one row. `helm status`
/// puts the revision under `version` and deploy state under `info`, so this is
/// not a flat key map. Pure over its input so it's testable on hand-built
/// fixtures.
pub fn project(value: &Value) -> [String; 6] {
    let info = value.get("info");
    [
        render_cell(value.get("name")),
        render_cell(value.get("namespace")),
        render_cell(value.get("version")), // helm calls the revision `version`
        render_cell(info.and_then(|i| i.get("status"))),
        render_cell(info.and_then(|i| i.get("last_deployed"))),
        notes_present(info).to_string(),
    ]
}

/// Whether the release carries a rendered NOTES.txt (non-empty `info.notes`).
fn notes_present(info: Option<&Value>) -> bool {
    info.and_then(|i| i.get("notes"))
        .and_then(Value::as_str)
        .is_some_and(|s| !s.trim().is_empty())
}

/// Parse the release object and emit a header + one TSV row. An empty/`{}`
/// body counts as "no results"; any object yields a row.
fn emit_tsv(writer: &mut BoundedWriter<'_>, stdout: &[u8]) -> Result<bool> {
    let text = String::from_utf8_lossy(stdout);
    let trimmed = text.trim();
    if trimmed.is_empty() || trimmed == "{}" {
        return Ok(false);
    }
    let value: Value =
        serde_json::from_str(trimmed).context("parsing `helm status -o json` output")?;
    if !value.is_object() {
        return Ok(false);
    }
    writer.write_decoration(&COLUMNS.join("\t"))?;
    writer.write_line(&project(&value).join("\t"))?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn bare() -> StatusArgs {
        StatusArgs {
            release: "cilium".to_string(),
            namespace: None,
            revision: None,
            show_resources: false,
            format: Format::Tsv,
            limit: None,
        }
    }

    #[test]
    fn default_argv_is_json_output_plus_release() {
        assert_eq!(build_argv(&bare()), vec!["--output", "json", "cilium"]);
    }

    #[test]
    fn flags_map_to_helm_argv_with_release_last() {
        let mut args = bare();
        args.revision = Some(16);
        args.show_resources = true;
        assert_eq!(
            build_argv(&args),
            vec![
                "--output",
                "json",
                "--revision",
                "16",
                "--show-resources",
                "cilium",
            ]
        );
    }

    fn release_fixture() -> Value {
        json!({
            "name": "cilium",
            "namespace": "kube-system",
            "version": 17,
            "info": {
                "last_deployed": "2026-05-15T03:58:47.289556028Z",
                "status": "deployed",
                "notes": "Cilium installed. Run `cilium status` to verify.",
            },
        })
    }

    #[test]
    fn project_pulls_nested_fields() {
        assert_eq!(
            project(&release_fixture()),
            [
                "cilium",
                "kube-system",
                "17", // numeric revision rendered as string
                "deployed",
                "2026-05-15T03:58:47.289556028Z",
                "true", // notes present
            ]
            .map(String::from)
        );
    }

    #[test]
    fn project_marks_absent_notes_false() {
        let mut v = release_fixture();
        v["info"].as_object_mut().unwrap().remove("notes");
        assert_eq!(project(&v)[5], "false");
    }

    #[test]
    fn project_marks_empty_notes_false() {
        let mut v = release_fixture();
        v["info"]["notes"] = json!("   \n  ");
        assert_eq!(project(&v)[5], "false");
    }

    #[test]
    fn project_renders_missing_fields_as_dash() {
        let v = json!({"name": "x"});
        let row = project(&v);
        assert_eq!(row[0], "x");
        assert_eq!(row[1], "-"); // namespace
        assert_eq!(row[2], "-"); // version
        assert_eq!(row[3], "-"); // status
        assert_eq!(row[4], "-"); // last_deployed
        assert_eq!(row[5], "false"); // notes_present
    }
}
