//! `sak k8s get <kind> [name]` — list or fetch resources of a given kind.
//!
//! Subsumes a separate `query` command via `--path`: pass an expression and
//! only the extracted value(s) are emitted, formatted by the same
//! [`crate::value::resolve_expression`] machinery the `json` and `config`
//! domains use.

use crate::output::Outcome;
use std::io;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Result, bail};
use clap::{Args, ValueEnum};
use kube::api::ListParams;
use kube::discovery::Scope;
use serde_json::Value;

use crate::k8s::{client, discovery, podtable};
use crate::output::{BoundedWriter, collapse_ws};
use crate::value::{format_value, resolve_expression};

/// Output shape for `sak k8s get` list/single output.
#[derive(Copy, Clone, PartialEq, Eq, ValueEnum, Default)]
pub enum OutputFormat {
    /// NDJSON list / pretty single object (the default, machine-friendly form)
    #[default]
    Json,
    /// kubectl-style column table (pods: NAME/READY/STATUS/RESTARTS/AGE; other
    /// kinds: NAME/AGE)
    Table,
    /// Like `table` but with extra columns (pods add IP and NODE)
    Wide,
}

#[derive(Args)]
#[command(
    about = "List or get Kubernetes resources",
    long_about = "List resources of a kind, or get a single resource by name.\n\n\
        Kinds may be supplied as `kubectl`-style shortnames (`po`, `deploy`, \
        `svc`, ...) or as full kind names (`Pod`, `Deployment`, ...). Common \
        builtins resolve via a hardcoded fast-path table; anything else falls \
        back to live cluster discovery (multi-second on big clusters).\n\n\
        Custom resources (CRDs) resolve through discovery by bare name \
        (`drivers`), `name.group` (`drivers.csi.ceph.io`), or full \
        `apiVersion/kind` (`csi.ceph.io/v1/Driver`). Use the qualified forms to \
        disambiguate a plural or kind that collides across groups.\n\n\
        Output:\n\n  \
        - List mode: NDJSON, one resource per line, sorted by (namespace, name).\n  \
        - Get mode: pretty-printed JSON for the single resource.\n  \
        - With --path: just the extracted value(s), one per resource.\n  \
        - With --columns: a TSV table, one row per object, header derived from \
        each column path (or `HEADER=path` to name it).\n  \
        - With -o table / -o wide: a kubectl-style column table. For pods the \
        columns are NAME/READY/STATUS/RESTARTS/AGE (wide adds IP and NODE); for \
        other kinds NAME/AGE. A NAMESPACE column is prepended with -A.\n\n\
        --path semantics: list output is NDJSON (one object per line), NOT a \
        kubectl-style List wrapper, so --path is applied to *each* object \
        independently. Use a per-object path like `.metadata.name`; a List-style \
        `.items[0].metadata.name` matches nothing (a hint is printed to stderr \
        when a path matches zero records). To feed the stream into `sak json`, \
        consume it per line, not as one document.\n\n\
        Exit codes follow sak convention: 0 = found, 1 = not found, 2 = error.",
    after_help = "\
Examples:
  sak k8s get pods                                List pods in the current namespace
  sak k8s get pods -A                             List pods across all namespaces
  sak k8s get pods -n kube-system                 List pods in kube-system
  sak k8s get deploy foo -n bar                   Get a single deployment
  sak k8s get deploy foo -n bar --path .spec.replicas
                                                  Extract one field from one resource
  sak k8s get pods -A --path .metadata.name       Names of all pods, one per line
  sak k8s get pods -l app=nginx                   Filter by label selector
  sak k8s get pods --field-selector status.phase=Running
  sak k8s get drivers.csi.ceph.io -A              List a CRD by name.group
  sak k8s get cephcluster.ceph.rook.io -A         CRD by singular name.group
  sak k8s get csi.ceph.io/v1/Driver -A            CRD by full apiVersion/kind
  sak k8s get pods -A -o wide                      Pod table with NODE/IP columns
  sak k8s get pods -n ctrlplugin -o table          NAME/READY/STATUS/RESTARTS/AGE
  sak k8s get pods -A --columns .metadata.name,.status.phase,.spec.hostNetwork
                                                  Project arbitrary fields to TSV
  sak k8s get pods -A --columns NAME=.metadata.name,HOST=.spec.hostNetwork
                                                  Name the columns explicitly"
)]
pub struct GetArgs {
    /// Resource kind (e.g. `pod`, `deployment`, `Lease`)
    pub kind: String,

    /// Resource name. Omit to list; supply for a single get.
    pub name: Option<String>,

    /// Namespace scope (default: cluster default from kubeconfig)
    #[arg(short, long, conflicts_with = "all_namespaces")]
    pub namespace: Option<String>,

    /// List across all namespaces (mutually exclusive with --namespace)
    #[arg(short = 'A', long)]
    pub all_namespaces: bool,

    /// Label selector (e.g. `app=nginx,tier=frontend`)
    #[arg(short = 'l', long)]
    pub selector: Option<String>,

    /// Field selector (e.g. `status.phase=Running`)
    #[arg(long)]
    pub field_selector: Option<String>,

    /// Extract a field via dot notation (`.spec.replicas`) or JSON Pointer
    /// (`/spec/replicas`)
    #[arg(long, conflicts_with_all = ["columns", "output"])]
    pub path: Option<String>,

    /// Project each object to a TSV row using comma-separated path expressions.
    /// Each column is `<path>` or `<HEADER>=<path>`; the default header is the
    /// path's leaf segment uppercased (e.g. `.metadata.name` → NAME). Missing
    /// values render as `-`.
    #[arg(long, value_delimiter = ',', conflicts_with = "output")]
    pub columns: Vec<String>,

    /// Output format: `json` (default), `table`, or `wide`
    #[arg(short = 'o', long, value_enum, default_value_t = OutputFormat::Json)]
    pub output: OutputFormat,

    /// Maximum number of output lines
    #[arg(long)]
    pub limit: Option<usize>,
}

pub async fn run(args: &GetArgs) -> Result<Outcome> {
    let client = client::build_client().await?;
    let (ar, caps) = discovery::resolve(&client, &args.kind).await?;

    // Validate scope vs flags before issuing any list/get call.
    if matches!(caps.scope, Scope::Cluster) {
        if args.namespace.is_some() {
            bail!(
                "kind {:?} is cluster-scoped — --namespace is not valid for it",
                ar.kind
            );
        }
        if args.all_namespaces {
            bail!(
                "kind {:?} is cluster-scoped — --all-namespaces is not valid for it",
                ar.kind
            );
        }
    }

    // Resolve effective namespace for the apiserver call.
    //
    // - cluster-scoped kinds: always None
    // - --all-namespaces: None (Api::all_with under the hood)
    // - --namespace ns:    Some(ns)
    // - neither (namespaced): Some(default_namespace())
    let effective_ns: Option<String> = match caps.scope {
        Scope::Cluster => None,
        Scope::Namespaced => {
            if args.all_namespaces {
                None
            } else if let Some(ns) = &args.namespace {
                Some(ns.clone())
            } else {
                Some(client.default_namespace().to_string())
            }
        }
    };

    // Gather the target object(s) up front so the same set can be rendered as
    // JSON, a TSV projection (--columns), or a column table (-o table|wide).
    let single = args.name.is_some();
    let objects: Vec<Value> = match &args.name {
        Some(name) => match client::get_dyn(&client, &ar, effective_ns.as_deref(), name).await? {
            Some(obj) => vec![serde_json::to_value(&obj)?],
            None => return Ok(Outcome::NotFound),
        },
        None => {
            let lp = build_list_params(args);
            let list = client::list_dyn(&client, &ar, effective_ns.as_deref(), &lp).await?;

            // Deterministic order per sak convention.
            let mut items = list.items;
            items.sort_by(|a, b| {
                let an = a.metadata.namespace.as_deref().unwrap_or("");
                let bn = b.metadata.namespace.as_deref().unwrap_or("");
                let aname = a.metadata.name.as_deref().unwrap_or("");
                let bname = b.metadata.name.as_deref().unwrap_or("");
                (an, aname).cmp(&(bn, bname))
            });
            items
                .iter()
                .map(serde_json::to_value)
                .collect::<Result<_, _>>()?
        }
    };

    let stdout = io::stdout();
    let handle = stdout.lock();
    let mut writer = BoundedWriter::new(handle, args.limit);

    // --columns and -o table|wide are each mutually exclusive with --path
    // (enforced by clap), so dispatch is a clean three-way split.
    if !args.columns.is_empty() {
        let specs = parse_column_specs(&args.columns);
        return emit_columns(&objects, &specs, &mut writer);
    }
    if matches!(args.output, OutputFormat::Table | OutputFormat::Wide) {
        let wide = matches!(args.output, OutputFormat::Wide);
        let is_pod = ar.kind == "Pod";
        return emit_table(&objects, is_pod, wide, args.all_namespaces, &mut writer);
    }

    // ── JSON (default) ──────────────────────────────────────────────────
    let mut wrote_any = false;
    // Count records seen and records the --path matched, so a path that
    // matches nothing across the whole stream can emit a helpful hint.
    let mut records = 0usize;
    let mut matched = 0usize;
    for value in &objects {
        let outcome = if let Some(expr) = &args.path {
            records += 1;
            let e = emit_path(value, expr, &mut writer)?;
            if e.matched {
                matched += 1;
            }
            e
        } else if single {
            // Single resource: pretty-printed JSON.
            emit_value(value, true, &mut writer)?
        } else {
            // NDJSON: compact, one resource per line.
            emit_value(value, false, &mut writer)?
        };
        wrote_any |= outcome.wrote;
        if outcome.stop {
            break;
        }
    }

    writer.flush()?;

    // A --path that matched zero of N (N > 0) records is almost always the
    // `.items[...]` / kubectl-List confusion — `get` output is NDJSON, one
    // object per line, so paths are relative to each object, not a wrapper.
    if let Some(expr) = &args.path
        && let Some(hint) = path_miss_hint(expr, records, matched)
    {
        eprintln!("{hint}");
    }

    if wrote_any {
        Ok(Outcome::Found)
    } else {
        Ok(Outcome::NotFound)
    }
}

/// Current unix time in whole seconds, for relative AGE computation. A clock at
/// or before the epoch (unreachable in practice) collapses to 0, which renders
/// every age as `0s` rather than panicking.
fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// One `--columns` entry: a display `header` and the path `expr` to resolve.
struct ColSpec {
    header: String,
    expr: String,
}

/// Parse `--columns` entries. Each is either `<path>` (header derived from the
/// path's leaf segment, uppercased) or `<HEADER>=<path>` (explicit header).
fn parse_column_specs(cols: &[String]) -> Vec<ColSpec> {
    cols.iter()
        .map(|c| match c.split_once('=') {
            Some((h, e)) => ColSpec {
                header: h.to_string(),
                expr: e.to_string(),
            },
            None => ColSpec {
                header: derive_header(c),
                expr: c.clone(),
            },
        })
        .collect()
}

/// Derive a column header from a path expression: take the leaf segment (after
/// the last `.`/`/`), strip any trailing `[index]`, and uppercase it.
/// `.metadata.name` → `NAME`, `/spec/hostNetwork` → `HOSTNETWORK`. Falls back to
/// the whole expression uppercased when there is no usable leaf.
fn derive_header(expr: &str) -> String {
    let leaf = expr.rsplit(['.', '/']).next().unwrap_or(expr);
    let leaf = leaf.split('[').next().unwrap_or(leaf).trim();
    if leaf.is_empty() {
        expr.to_uppercase()
    } else {
        leaf.to_uppercase()
    }
}

/// Render one cell for `--columns`: resolve `expr` against `value`, render the
/// result as a single TSV-safe string. A missing path or JSON null becomes `-`;
/// scalars render raw; arrays/objects render as compact JSON. Newlines and tabs
/// are collapsed so a value can't break the one-row-per-object TSV contract.
fn render_cell(value: &Value, expr: &str) -> Result<String> {
    Ok(match resolve_expression(value, expr)? {
        None | Some(Value::Null) => "-".to_string(),
        Some(v) => collapse_ws(&format_value(v, true, false)),
    })
}

/// Emit a `--columns` TSV table: a header row (decoration, not counted toward
/// `--limit`) followed by one row per object. An empty object set writes nothing
/// and is `NotFound`, matching the standard "no results" convention.
fn emit_columns(
    objects: &[Value],
    specs: &[ColSpec],
    writer: &mut BoundedWriter<'_>,
) -> Result<Outcome> {
    if objects.is_empty() {
        writer.flush()?;
        return Ok(Outcome::NotFound);
    }
    let header = specs
        .iter()
        .map(|s| s.header.as_str())
        .collect::<Vec<_>>()
        .join("\t");
    writer.write_decoration(&header)?;

    let mut wrote_any = false;
    for value in objects {
        let cells = specs
            .iter()
            .map(|s| render_cell(value, &s.expr))
            .collect::<Result<Vec<_>>>()?;
        if !writer.write_line(&cells.join("\t"))? {
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

/// Emit a kubectl-style column table. For pods the columns are
/// NAME/READY/STATUS/RESTARTS/AGE (wide adds IP/NODE); other kinds get NAME/AGE.
/// A NAMESPACE column is prepended when listing across namespaces. Output is
/// tab-separated (sak's TSV convention), not space-aligned. Header is decoration;
/// an empty object set writes nothing and is `NotFound`.
fn emit_table(
    objects: &[Value],
    is_pod: bool,
    wide: bool,
    all_namespaces: bool,
    writer: &mut BoundedWriter<'_>,
) -> Result<Outcome> {
    if objects.is_empty() {
        writer.flush()?;
        return Ok(Outcome::NotFound);
    }
    let now = now_unix();

    let mut headers: Vec<&str> = Vec::new();
    if all_namespaces {
        headers.push("NAMESPACE");
    }
    if is_pod {
        headers.extend(["NAME", "READY", "STATUS", "RESTARTS", "AGE"]);
        if wide {
            headers.extend(["IP", "NODE"]);
        }
    } else {
        headers.extend(["NAME", "AGE"]);
    }
    writer.write_decoration(&headers.join("\t"))?;

    let mut wrote_any = false;
    for value in objects {
        let mut cells: Vec<String> = Vec::new();
        if all_namespaces {
            cells.push(meta_str(value, "namespace"));
        }
        let age = podtable::translate_timestamp(creation_ts(value), now);
        if is_pod {
            cells.push(meta_str(value, "name"));
            cells.push(podtable::pod_ready(value));
            cells.push(podtable::pod_status(value));
            cells.push(podtable::pod_restarts(value).to_string());
            cells.push(age);
            if wide {
                cells.push(podtable::pod_ip(value));
                cells.push(podtable::pod_node(value));
            }
        } else {
            cells.push(meta_str(value, "name"));
            cells.push(age);
        }
        if !writer.write_line(&cells.join("\t"))? {
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

/// Read a `metadata.<key>` string, defaulting to `-` when absent or empty.
fn meta_str(value: &Value, key: &str) -> String {
    value
        .get("metadata")
        .and_then(|m| m.get(key))
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .unwrap_or("-")
        .to_string()
}

/// Borrow `metadata.creationTimestamp` if present.
fn creation_ts(value: &Value) -> Option<&str> {
    value
        .get("metadata")
        .and_then(|m| m.get("creationTimestamp"))
        .and_then(Value::as_str)
}

/// Build `ListParams` from the user-supplied selector flags.
fn build_list_params(args: &GetArgs) -> ListParams {
    let mut lp = ListParams::default();
    if let Some(sel) = &args.selector {
        lp = lp.labels(sel);
    }
    if let Some(sel) = &args.field_selector {
        lp = lp.fields(sel);
    }
    lp
}

/// Outcome of one `emit_*` call. `wrote` records whether at least one line
/// made it past the `BoundedWriter` limit; `stop` records whether the limit
/// fired mid-emit and the caller should bail out of its loop; `matched`
/// records whether a `--path` expression resolved to a value for this record
/// (distinct from `wrote`, which can be false at the `--limit` boundary even
/// on a match).
struct Emit {
    wrote: bool,
    stop: bool,
    matched: bool,
}

/// Emit a JSON value through the bounded writer. `pretty=true` writes a
/// multi-line pretty-print (each line counts toward `--limit`); `pretty=false`
/// writes a single compact line (NDJSON).
fn emit_value(value: &Value, pretty: bool, writer: &mut BoundedWriter<'_>) -> Result<Emit> {
    let formatted = format_value(value, false, pretty);
    let mut wrote = false;
    for line in formatted.split('\n') {
        if !writer.write_line(line)? {
            return Ok(Emit {
                wrote,
                stop: true,
                matched: true,
            });
        }
        wrote = true;
    }
    Ok(Emit {
        wrote,
        stop: false,
        matched: true,
    })
}

/// Resolve `expr` against `value` and emit the result, if any. A missing
/// path is *not* an error — it just produces no output for this resource,
/// matching `value::resolve_expression`'s `Option` semantics.
fn emit_path(value: &Value, expr: &str, writer: &mut BoundedWriter<'_>) -> Result<Emit> {
    let Some(extracted) = resolve_expression(value, expr)? else {
        return Ok(Emit {
            wrote: false,
            stop: false,
            matched: false,
        });
    };
    emit_value(extracted, false, writer)
}

/// Build the stderr hint shown when a `--path` expression matched none of the
/// records it was applied to. Returns `None` when there is nothing to warn
/// about (no records seen, or at least one match). The hint steers users away
/// from the common `.items[...]` / kubectl-List mistake — `get` output is
/// NDJSON, so `--path` is per-object, not against a List wrapper.
fn path_miss_hint(expr: &str, records: usize, matched: usize) -> Option<String> {
    if records == 0 || matched > 0 {
        return None;
    }
    Some(format!(
        "sak: --path {expr:?} matched 0 of {records} record(s)\n\
         note: `sak k8s get` output is NDJSON (one object per line) — --path is \
         applied to each object, not a kubectl-style List wrapper. Use a per-object \
         path like `.metadata.name`, not `.items[0].metadata.name`."
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hint_fires_on_zero_matches() {
        let h = path_miss_hint(".items[0].metadata.name", 7, 0).expect("hint expected");
        assert!(h.contains("matched 0 of 7 record(s)"));
        assert!(h.contains("NDJSON"));
        assert!(h.contains(".metadata.name"));
    }

    #[test]
    fn no_hint_when_some_matched() {
        assert!(path_miss_hint(".metadata.name", 7, 3).is_none());
    }

    #[test]
    fn no_hint_when_no_records() {
        // An empty list (no records) should not look like a path mistake.
        assert!(path_miss_hint(".items[0].metadata.name", 0, 0).is_none());
    }

    #[test]
    fn derive_header_from_leaf() {
        assert_eq!(derive_header(".metadata.name"), "NAME");
        assert_eq!(derive_header("/spec/hostNetwork"), "HOSTNETWORK");
        assert_eq!(derive_header(".spec.containers[0]"), "CONTAINERS");
        // No usable leaf → whole expression uppercased.
        assert_eq!(derive_header("."), ".");
    }

    #[test]
    fn parse_specs_explicit_and_derived_headers() {
        let specs = parse_column_specs(&[
            ".metadata.name".to_string(),
            "HOST=.spec.hostNetwork".to_string(),
        ]);
        assert_eq!(specs.len(), 2);
        assert_eq!(specs[0].header, "NAME");
        assert_eq!(specs[0].expr, ".metadata.name");
        assert_eq!(specs[1].header, "HOST");
        assert_eq!(specs[1].expr, ".spec.hostNetwork");
    }

    #[test]
    fn render_cell_renders_scalars_missing_and_complex() {
        let obj = serde_json::json!({
            "metadata": {"name": "p"},
            "spec": {"hostNetwork": true, "containers": [{"name": "c"}]},
            "status": {"phase": "Running"}
        });
        // String scalar comes through raw (no JSON quotes).
        assert_eq!(render_cell(&obj, ".metadata.name").unwrap(), "p");
        // Boolean scalar.
        assert_eq!(render_cell(&obj, ".spec.hostNetwork").unwrap(), "true");
        // Missing path → dash.
        assert_eq!(render_cell(&obj, ".spec.missing").unwrap(), "-");
        // Array → compact JSON.
        assert_eq!(
            render_cell(&obj, ".spec.containers").unwrap(),
            r#"[{"name":"c"}]"#
        );
    }

    #[test]
    fn render_cell_collapses_whitespace() {
        let obj = serde_json::json!({"metadata": {"annotations": {"k": "a\tb\nc"}}});
        // Tabs/newlines in a value must not break the TSV row.
        let cell = render_cell(&obj, ".metadata.annotations.k").unwrap();
        assert!(!cell.contains('\t'));
        assert!(!cell.contains('\n'));
        assert_eq!(cell, "a b c");
    }
}
