use std::process::ExitCode;

use anyhow::{Context, Result};
use clap::{Args, ValueEnum};
use serde_json::Value;

use crate::helm::client::{self, Conn};
use crate::helm::{Format, emit_to_stdout};
use crate::output::{BoundedWriter, collapse_ws};

/// Fixed TSV column set, in emission order. Matches the snake_case keys
/// `helm list -o json` produces, so each column is a direct key lookup.
const COLUMNS: [&str; 7] = [
    "name",
    "namespace",
    "revision",
    "updated",
    "status",
    "chart",
    "app_version",
];

/// `helm list` status filters. `helm` exposes these as individual boolean
/// flags rather than a `--status <value>` option, so sak maps a single
/// `--status` value onto the matching flag. `--status pending` matches every
/// `pending-*` state (install / upgrade / rollback), mirroring `helm
/// --pending`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum StatusFilter {
    Deployed,
    Failed,
    Pending,
    Superseded,
    Uninstalled,
    Uninstalling,
}

impl StatusFilter {
    /// The bare `helm list` flag word (without `--`).
    fn as_helm(self) -> &'static str {
        match self {
            StatusFilter::Deployed => "deployed",
            StatusFilter::Failed => "failed",
            StatusFilter::Pending => "pending",
            StatusFilter::Superseded => "superseded",
            StatusFilter::Uninstalled => "uninstalled",
            StatusFilter::Uninstalling => "uninstalling",
        }
    }
}

#[derive(Args)]
#[command(
    about = "List Helm releases as TSV (read-only)",
    long_about = "List Helm releases via `helm list -o json` and emit one TSV row per \
        release with the columns \
        name, namespace, revision, updated, status, chart, app_version.\n\n\
        By default `helm` lists releases in the current kubeconfig namespace; \
        use `--all-namespaces`/`-A` to fan out across the cluster, or \
        `--namespace` to target one. `--status` maps to `helm`'s per-status \
        flags (it has no `--status <value>` option); `--status pending` covers \
        every pending-* state. `--filter` is forwarded as `helm`'s name regex. \
        Use `--format json` for the raw `helm` JSON array.\n\n\
        Cluster, auth, and namespace resolution are whatever `helm` itself \
        uses (`KUBECONFIG` / `~/.kube/config`).\n\n\
        Exit status: 0 when at least one release is listed, 1 when none match, \
        2 on error.",
    after_help = "\
Examples:
  sak helm list                                Releases in the current namespace
  sak helm list -A                             Releases across all namespaces
  sak helm list --namespace kube-system
  sak helm list --status failed -A             Failed releases cluster-wide
  sak helm list --filter '^ingress-'           Releases whose name matches a regex
  sak helm list --format json                  Raw helm JSON array"
)]
pub struct ListArgs {
    /// Single namespace to list (default: the kubeconfig's current namespace)
    #[arg(
        long,
        short = 'n',
        value_name = "NS",
        conflicts_with = "all_namespaces"
    )]
    pub namespace: Option<String>,

    /// List releases across all namespaces
    #[arg(long = "all-namespaces", short = 'A')]
    pub all_namespaces: bool,

    /// Filter by release status
    #[arg(long, value_enum)]
    pub status: Option<StatusFilter>,

    /// Filter release names by regex (forwarded to `helm --filter`)
    #[arg(long, value_name = "REGEX")]
    pub filter: Option<String>,

    /// Output format
    #[arg(long, value_enum, default_value_t = Format::Tsv)]
    pub format: Format,

    /// Maximum number of output lines (bounds output, not the helm fetch)
    #[arg(long)]
    pub limit: Option<usize>,
}

/// One projected release row.
#[derive(Debug, PartialEq, Eq)]
pub struct Row {
    pub cells: [String; 7],
}

impl Row {
    fn to_tsv(&self) -> String {
        self.cells.join("\t")
    }
}

pub fn run(args: &ListArgs) -> Result<ExitCode> {
    let argv = build_argv(args);
    let argv_refs: Vec<&str> = argv.iter().map(String::as_str).collect();
    let conn = Conn {
        namespace: args.namespace.as_deref(),
        ..Default::default()
    };
    let stdout = client::invoke_ok("list", None, &argv_refs, conn)?;
    emit_to_stdout(&stdout, args.format, args.limit, "[]", emit_tsv)
}

/// Assemble the `helm list` argv (everything but the connection flags, which
/// the chokepoint applies from `Conn`).
fn build_argv(args: &ListArgs) -> Vec<String> {
    let mut v = vec!["--output".to_string(), "json".to_string()];
    if args.all_namespaces {
        v.push("--all-namespaces".to_string());
    }
    if let Some(status) = args.status {
        v.push(format!("--{}", status.as_helm()));
    }
    if let Some(filter) = &args.filter {
        v.push("--filter".to_string());
        v.push(filter.clone());
    }
    v
}

/// Project `helm list -o json`'s top-level array into rows. A non-array value
/// (or non-object elements) yields no rows; missing / null fields render `-`.
/// Pure over its input so it's testable on hand-built fixtures.
pub fn walk(value: &Value) -> Vec<Row> {
    let Some(arr) = value.as_array() else {
        return Vec::new();
    };
    arr.iter()
        .filter(|rec| rec.is_object())
        .map(|rec| Row {
            cells: COLUMNS.map(|col| cell(rec, col)),
        })
        .collect()
}

/// Render one release field to a cell: missing/null → `-`, scalars verbatim
/// (whitespace collapsed), anything structured → compact JSON.
fn cell(rec: &Value, key: &str) -> String {
    match rec.get(key) {
        None | Some(Value::Null) => "-".to_string(),
        Some(Value::String(s)) => collapse_ws(s),
        Some(Value::Number(n)) => n.to_string(),
        Some(Value::Bool(b)) => b.to_string(),
        Some(other) => collapse_ws(&serde_json::to_string(other).unwrap_or_default()),
    }
}

/// Parse `helm`'s JSON array, project rows, and emit a header + TSV rows. The
/// header is decoration (not counted toward `--limit`); no releases → no
/// results.
fn emit_tsv(writer: &mut BoundedWriter<'_>, stdout: &[u8]) -> Result<bool> {
    let text = String::from_utf8_lossy(stdout);
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Ok(false);
    }
    let value: Value =
        serde_json::from_str(trimmed).context("parsing `helm list -o json` output")?;
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

    fn bare() -> ListArgs {
        ListArgs {
            namespace: None,
            all_namespaces: false,
            status: None,
            filter: None,
            format: Format::Tsv,
            limit: None,
        }
    }

    #[test]
    fn default_argv_requests_json_only() {
        assert_eq!(build_argv(&bare()), vec!["--output", "json"]);
    }

    #[test]
    fn flags_map_to_helm_argv() {
        let mut args = bare();
        args.all_namespaces = true;
        args.status = Some(StatusFilter::Failed);
        args.filter = Some("^ingress-".to_string());
        assert_eq!(
            build_argv(&args),
            vec![
                "--output",
                "json",
                "--all-namespaces",
                "--failed",
                "--filter",
                "^ingress-",
            ]
        );
    }

    #[test]
    fn pending_maps_to_pending_flag() {
        let mut args = bare();
        args.status = Some(StatusFilter::Pending);
        assert!(build_argv(&args).contains(&"--pending".to_string()));
    }

    #[test]
    fn walk_projects_columns_in_order() {
        let v = json!([{
            "name": "cilium",
            "namespace": "kube-system",
            "revision": 3,
            "updated": "2026-05-01 12:00:00",
            "status": "deployed",
            "chart": "cilium-1.15.0",
            "app_version": "1.15.0"
        }]);
        let rows = walk(&v);
        assert_eq!(rows.len(), 1);
        assert_eq!(
            rows[0].cells,
            [
                "cilium",
                "kube-system",
                "3", // numeric revision rendered as string
                "2026-05-01 12:00:00",
                "deployed",
                "cilium-1.15.0",
                "1.15.0",
            ]
            .map(String::from)
        );
    }

    #[test]
    fn walk_renders_missing_and_null_as_dash() {
        let v = json!([{"name": "x", "status": null}]);
        let rows = walk(&v);
        assert_eq!(rows[0].cells[0], "x");
        assert_eq!(rows[0].cells[1], "-"); // namespace missing
        assert_eq!(rows[0].cells[4], "-"); // status null
    }

    #[test]
    fn walk_handles_revision_as_string() {
        // Some helm versions emit revision as a quoted string.
        let v = json!([{"name": "x", "revision": "7"}]);
        assert_eq!(walk(&v)[0].cells[2], "7");
    }

    #[test]
    fn walk_empty_array_yields_no_rows() {
        assert!(walk(&json!([])).is_empty());
    }

    #[test]
    fn walk_non_array_yields_no_rows() {
        assert!(walk(&json!({"name": "x"})).is_empty());
        assert!(walk(&json!("nope")).is_empty());
    }

    #[test]
    fn walk_skips_non_object_elements() {
        let v = json!([{"name": "a"}, "garbage", 42, {"name": "b"}]);
        let rows = walk(&v);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].cells[0], "a");
        assert_eq!(rows[1].cells[0], "b");
    }

    #[test]
    fn cell_collapses_whitespace() {
        let rec = json!({"chart": "foo\tbar\nbaz"});
        assert_eq!(cell(&rec, "chart"), "foo bar baz");
    }
}
