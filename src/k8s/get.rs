//! `sak k8s get <kind> [name]` — list or fetch resources of a given kind.
//!
//! Subsumes a separate `query` command via `--path`: pass an expression and
//! only the extracted value(s) are emitted, formatted by the same
//! [`crate::value::resolve_expression`] machinery the `json` and `config`
//! domains use.

use crate::output::Outcome;
use std::io;

use anyhow::{Result, bail};
use clap::Args;
use kube::api::ListParams;
use kube::discovery::Scope;
use serde_json::Value;

use crate::k8s::{client, discovery};
use crate::output::BoundedWriter;
use crate::value::{format_value, resolve_expression};

#[derive(Args)]
#[command(
    about = "List or get Kubernetes resources",
    long_about = "List resources of a kind, or get a single resource by name.\n\n\
        Kinds may be supplied as `kubectl`-style shortnames (`po`, `deploy`, \
        `svc`, ...) or as full kind names (`Pod`, `Deployment`, ...). Common \
        builtins resolve via a hardcoded fast-path table; anything else falls \
        back to live cluster discovery (multi-second on big clusters).\n\n\
        Output:\n\n  \
        - List mode: NDJSON, one resource per line, sorted by (namespace, name).\n  \
        - Get mode: pretty-printed JSON for the single resource.\n  \
        - With --path: just the extracted value(s), one per resource.\n\n\
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
  sak k8s get pods --field-selector status.phase=Running"
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
    #[arg(long)]
    pub path: Option<String>,

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

    let stdout = io::stdout();
    let handle = stdout.lock();
    let mut writer = BoundedWriter::new(handle, args.limit);

    let mut wrote_any = false;
    // Count records seen and records the --path matched, so a path that
    // matches nothing across the whole stream can emit a helpful hint.
    let mut records = 0usize;
    let mut matched = 0usize;
    match &args.name {
        // ── Single get ────────────────────────────────────────────────────
        Some(name) => {
            let obj = client::get_dyn(&client, &ar, effective_ns.as_deref(), name).await?;
            let Some(obj) = obj else {
                writer.flush()?;
                return Ok(Outcome::NotFound);
            };
            let value: Value = serde_json::to_value(&obj)?;

            let outcome = if let Some(expr) = &args.path {
                records += 1;
                let e = emit_path(&value, expr, &mut writer)?;
                if e.matched {
                    matched += 1;
                }
                e
            } else {
                // Single resource: pretty-printed JSON.
                emit_value(&value, true, &mut writer)?
            };
            wrote_any |= outcome.wrote;
        }

        // ── List ──────────────────────────────────────────────────────────
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

            for obj in &items {
                let value: Value = serde_json::to_value(obj)?;
                let outcome = if let Some(expr) = &args.path {
                    records += 1;
                    let e = emit_path(&value, expr, &mut writer)?;
                    if e.matched {
                        matched += 1;
                    }
                    e
                } else {
                    // NDJSON: compact, one resource per line.
                    emit_value(&value, false, &mut writer)?
                };
                wrote_any |= outcome.wrote;
                if outcome.stop {
                    break;
                }
            }
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
}
