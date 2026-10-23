//! `sak k8s top pods|nodes` — resource usage from metrics-server.
//!
//! Reads `metrics.k8s.io/v1beta1` PodMetrics/NodeMetrics (not core Pods/Nodes:
//! those carry no usage data). Quantity strings are parsed to base units,
//! summed per pod across containers, and re-emitted in kubectl's published
//! shape (millicores, whole MiB) — the raw metrics strings are not normalized
//! beyond that summation.

use crate::output::Outcome;
use std::collections::HashMap;
use std::io;

use anyhow::{Context, Result, bail};
use clap::{Args, Subcommand};
use kube::api::ListParams;
use kube::discovery::Scope;
use serde_json::Value;

use crate::k8s::{client, discovery};
use crate::output::BoundedWriter;

#[derive(Args)]
#[command(
    about = "Show CPU/memory usage from metrics-server",
    long_about = "Show live CPU/memory usage from metrics-server, like `kubectl top`.\n\n\
        `top pods` sums each pod's containers and prints \
        `POD<TAB>CPU<TAB>MEMORY` (POD is `namespace/name`), sorted by \
        (namespace, name). `top nodes` prints \
        `NODE<TAB>CPU<TAB>MEMORY<TAB>CPU%<TAB>MEMORY%`, sorted by name; the \
        percent columns divide usage by the node's allocatable (a failed or \
        missing allocatable lookup renders `-` rather than failing).\n\n\
        Totals print in kubectl's shape — integer millicores (`250m`) and \
        whole MiB (`128Mi`) — summed across a pod's containers. Requires \
        metrics-server; without its API group the command fails with a \
        pointer to installing it.",
    after_help = "\
Examples:
  sak k8s top pods                             Usage in the current namespace
  sak k8s top pods -A --limit 10               Top 10 pods cluster-wide
  sak k8s top pods -n kube-system -l app=cilium Filter by namespace and labels
  sak k8s top nodes                            Usage with allocatable percents"
)]
pub struct TopArgs {
    #[command(subcommand)]
    pub command: TopCommand,
}

#[derive(Subcommand)]
pub enum TopCommand {
    /// Per-pod CPU/memory summed across containers
    Pods(TopPodsArgs),
    /// Per-node CPU/memory with allocatable percentages
    Nodes(TopNodesArgs),
}

#[derive(Args)]
pub struct TopPodsArgs {
    /// Namespace scope
    #[arg(short, long, conflicts_with = "all_namespaces")]
    pub namespace: Option<String>,

    /// List across all namespaces
    #[arg(short = 'A', long)]
    pub all_namespaces: bool,

    /// Label selector (e.g. `app=nginx`)
    #[arg(short = 'l', long)]
    pub selector: Option<String>,

    /// Maximum number of output lines
    #[arg(long)]
    pub limit: Option<usize>,
}

#[derive(Args)]
pub struct TopNodesArgs {
    /// Maximum number of output lines
    #[arg(long)]
    pub limit: Option<usize>,
}

/// Parse a Kubernetes quantity string to base units (cores for CPU, bytes
/// for memory). Handles decimal SI (`n`, `u`, `m`, `k`, `M`, …), binary SI
/// (`Ki` … `Ei`), and decimal exponents (`129e6`). Pure and unit-tested.
fn parse_quantity(s: &str) -> Result<f64> {
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() && (b[i].is_ascii_digit() || b[i] == b'.') {
        i += 1;
    }
    // A trailing `e`/`E` starts a decimal exponent only when a signed or
    // unsigned digit run follows (`1e3`, `1E-6`); otherwise it starts the
    // suffix (`1E` is one exa, `1Ei` one exbibyte).
    if i < b.len() && (b[i] == b'e' || b[i] == b'E') {
        let mut j = i + 1;
        if j < b.len() && (b[j] == b'+' || b[j] == b'-') {
            j += 1;
        }
        if j < b.len() && b[j].is_ascii_digit() {
            while j < b.len() && b[j].is_ascii_digit() {
                j += 1;
            }
            let num: f64 = s[..j]
                .parse()
                .with_context(|| format!("invalid quantity {s:?}"))?;
            return apply_suffix(num, &s[j..]);
        }
    }
    if i == 0 {
        bail!("invalid quantity {s:?}");
    }
    let num: f64 = s[..i]
        .parse()
        .with_context(|| format!("invalid quantity {s:?}"))?;
    apply_suffix(num, &s[i..])
}

fn apply_suffix(num: f64, suffix: &str) -> Result<f64> {
    let factor: f64 = match suffix {
        "" => 1.0,
        "n" => 1e-9,
        "u" => 1e-6,
        "m" => 1e-3,
        "k" => 1e3,
        "M" => 1e6,
        "G" => 1e9,
        "T" => 1e12,
        "P" => 1e15,
        "E" => 1e18,
        "Ki" => 1024.0,
        "Mi" => 1024.0 * 1024.0,
        "Gi" => 1024.0 * 1024.0 * 1024.0,
        "Ti" => 1024.0f64.powi(4),
        "Pi" => 1024.0f64.powi(5),
        "Ei" => 1024.0f64.powi(6),
        _ => bail!("unknown quantity suffix {suffix:?}"),
    };
    Ok(num * factor)
}

/// Whole millicores, kubectl style (`250m`).
fn format_millicores(cores: f64) -> String {
    format!("{}m", (cores * 1000.0).round() as i64)
}

/// Whole MiB, kubectl style (`128Mi`).
fn format_mebibytes(bytes: f64) -> String {
    const MIB: f64 = 1024.0 * 1024.0;
    format!("{}Mi", (bytes / MIB).round() as i64)
}

/// Sum `containers[*].usage` of one PodMetrics object into (cpu_cores,
/// memory_bytes). Missing containers or fields count as zero.
fn pod_totals(item: &Value) -> (f64, f64) {
    let mut cpu = 0.0;
    let mut memory = 0.0;
    if let Some(containers) = item.get("containers").and_then(Value::as_array) {
        for c in containers {
            if let Some(usage) = c.get("usage") {
                if let Some(s) = usage.get("cpu").and_then(Value::as_str)
                    && let Ok(v) = parse_quantity(s)
                {
                    cpu += v;
                }
                if let Some(s) = usage.get("memory").and_then(Value::as_str)
                    && let Ok(v) = parse_quantity(s)
                {
                    memory += v;
                }
            }
        }
    }
    (cpu, memory)
}

/// Usage of one NodeMetrics object, or `None` when either field is missing
/// or unparsable.
fn node_usage(item: &Value) -> Option<(f64, f64)> {
    let usage = item.get("usage")?;
    let cpu = parse_quantity(usage.get("cpu")?.as_str()?).ok()?;
    let memory = parse_quantity(usage.get("memory")?.as_str()?).ok()?;
    Some((cpu, memory))
}

/// Percentage string for `usage / allocatable`, or `-` when the allocatable
/// is missing, unparsable, or zero.
fn percent(usage: f64, allocatable: Option<f64>) -> String {
    match allocatable {
        Some(a) if a > 0.0 => format!("{}%", (usage / a * 100.0).round() as i64),
        _ => "-".to_string(),
    }
}

/// Allocatable cpu/memory per node name from a core `Node` list. Pure over
/// the parsed value so the join is unit-testable without a cluster.
fn allocatable_map(nodes: &[Value]) -> HashMap<String, (f64, f64)> {
    let mut out = HashMap::new();
    for n in nodes {
        let Some(name) = n
            .get("metadata")
            .and_then(|m| m.get("name"))
            .and_then(Value::as_str)
        else {
            continue;
        };
        let alloc = match n.get("status").and_then(|s| s.get("allocatable")) {
            Some(a) => a,
            None => continue,
        };
        let cpu = alloc
            .get("cpu")
            .and_then(Value::as_str)
            .and_then(|s| parse_quantity(s).ok());
        let memory = alloc
            .get("memory")
            .and_then(Value::as_str)
            .and_then(|s| parse_quantity(s).ok());
        if let (Some(c), Some(m)) = (cpu, memory) {
            out.insert(name.to_string(), (c, m));
        }
    }
    out
}

const METRICS_MISSING: &str =
    "metrics.k8s.io API group not available — is metrics-server installed?";

pub async fn run(args: &TopArgs) -> Result<Outcome> {
    let client = client::build_client().await?;
    match &args.command {
        TopCommand::Pods(pods) => run_pods(&client, pods).await,
        TopCommand::Nodes(nodes) => run_nodes(&client, nodes).await,
    }
}

async fn run_pods(client: &kube::Client, args: &TopPodsArgs) -> Result<Outcome> {
    let (ar, caps) = discovery::resolve(client, "podmetrics")
        .await
        .context(METRICS_MISSING)?;
    if matches!(caps.scope, Scope::Cluster) {
        bail!("expected PodMetrics to be namespaced");
    }
    let effective_ns: Option<String> = if args.all_namespaces {
        None
    } else if let Some(ns) = &args.namespace {
        Some(ns.clone())
    } else {
        Some(client.default_namespace().to_string())
    };

    let mut lp = ListParams::default();
    if let Some(sel) = &args.selector {
        lp = lp.labels(sel);
    }
    let list = client::list_dyn(client, &ar, effective_ns.as_deref(), &lp).await?;

    let mut rows: Vec<(String, String, String, String)> = Vec::new();
    for obj in &list.items {
        let value: Value = serde_json::to_value(obj)?;
        let ns = value
            .get("metadata")
            .and_then(|m| m.get("namespace"))
            .and_then(Value::as_str)
            .unwrap_or("");
        let name = value
            .get("metadata")
            .and_then(|m| m.get("name"))
            .and_then(Value::as_str)
            .unwrap_or("");
        let (cpu, memory) = pod_totals(&value);
        rows.push((
            ns.to_string(),
            name.to_string(),
            format_millicores(cpu),
            format_mebibytes(memory),
        ));
    }
    rows.sort();
    if rows.is_empty() {
        return Ok(Outcome::NotFound);
    }

    let stdout = io::stdout();
    let handle = stdout.lock();
    let mut writer = BoundedWriter::new(handle, args.limit);
    writer.write_decoration("POD\tCPU\tMEMORY")?;
    for (ns, name, cpu, memory) in &rows {
        let pod = if ns.is_empty() {
            name.clone()
        } else {
            format!("{ns}/{name}")
        };
        if !writer.write_line(&format!("{pod}\t{cpu}\t{memory}"))? {
            break;
        }
    }
    writer.flush()?;
    Ok(Outcome::Found)
}

async fn run_nodes(client: &kube::Client, args: &TopNodesArgs) -> Result<Outcome> {
    let (ar, _caps) = discovery::resolve(client, "nodemetrics")
        .await
        .context(METRICS_MISSING)?;
    let list = client::list_dyn(client, &ar, None, &ListParams::default()).await?;

    // Allocatable comes from core Nodes; a failed lookup degrades to `-`
    // percent columns rather than failing the command.
    let alloc: HashMap<String, (f64, f64)> = match discovery::resolve(client, "node").await {
        Ok((node_ar, _)) => {
            match client::list_dyn(client, &node_ar, None, &ListParams::default()).await {
                Ok(nodes) => {
                    let values: Vec<Value> = nodes
                        .items
                        .iter()
                        .filter_map(|o| serde_json::to_value(o).ok())
                        .collect();
                    allocatable_map(&values)
                }
                Err(_) => HashMap::new(),
            }
        }
        Err(_) => HashMap::new(),
    };

    let mut rows: Vec<(String, String, String, String, String)> = Vec::new();
    for obj in &list.items {
        let value: Value = serde_json::to_value(obj)?;
        let name = value
            .get("metadata")
            .and_then(|m| m.get("name"))
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let Some((cpu, memory)) = node_usage(&value) else {
            continue;
        };
        let (ac, am) = alloc.get(&name).copied().unzip();
        rows.push((
            name,
            format_millicores(cpu),
            format_mebibytes(memory),
            percent(cpu, ac),
            percent(memory, am),
        ));
    }
    rows.sort();
    if rows.is_empty() {
        return Ok(Outcome::NotFound);
    }

    let stdout = io::stdout();
    let handle = stdout.lock();
    let mut writer = BoundedWriter::new(handle, args.limit);
    writer.write_decoration("NODE\tCPU\tMEMORY\tCPU%\tMEMORY%")?;
    for (name, cpu, memory, cpu_p, mem_p) in &rows {
        if !writer.write_line(&format!("{name}\t{cpu}\t{memory}\t{cpu_p}\t{mem_p}"))? {
            break;
        }
    }
    writer.flush()?;
    Ok(Outcome::Found)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parse_millicores_and_plain() {
        assert_eq!(parse_quantity("120m").unwrap(), 0.12);
        assert_eq!(parse_quantity("0").unwrap(), 0.0);
        assert_eq!(parse_quantity("2").unwrap(), 2.0);
        assert_eq!(parse_quantity("1.5").unwrap(), 1.5);
    }

    #[test]
    fn parse_binary_suffixes() {
        assert_eq!(parse_quantity("256Mi").unwrap(), 256.0 * 1024.0 * 1024.0);
        assert_eq!(parse_quantity("1Gi").unwrap(), 1024.0f64.powi(3));
        assert_eq!(parse_quantity("512Ki").unwrap(), 512.0 * 1024.0);
    }

    #[test]
    fn parse_decimal_suffixes_and_exponents() {
        assert_eq!(parse_quantity("1k").unwrap(), 1000.0);
        assert_eq!(parse_quantity("500M").unwrap(), 500e6);
        assert_eq!(parse_quantity("129e6").unwrap(), 129e6);
        assert_eq!(parse_quantity("1E3").unwrap(), 1000.0);
        // `1E` is one exa, not an exponent: E needs trailing digits.
        assert_eq!(parse_quantity("1E").unwrap(), 1e18);
        assert_eq!(parse_quantity("1Ei").unwrap(), 1024.0f64.powi(6));
    }

    #[test]
    fn parse_rejects_unknown() {
        assert!(parse_quantity("").is_err());
        assert!(parse_quantity("abc").is_err());
        assert!(parse_quantity("10X").is_err());
        assert!(parse_quantity("m").is_err());
    }

    #[test]
    fn format_totals_kubectl_style() {
        assert_eq!(format_millicores(0.25), "250m");
        assert_eq!(format_millicores(0.0), "0m");
        assert_eq!(format_mebibytes(128.0 * 1024.0 * 1024.0), "128Mi");
    }

    #[test]
    fn pod_totals_sum_containers() {
        // 120m + 80m of cpu, 256Mi + 1Gi of memory.
        let item = json!({
            "containers": [
                {"usage": {"cpu": "120m", "memory": "256Mi"}},
                {"usage": {"cpu": "80m", "memory": "1Gi"}},
            ]
        });
        let (cpu, memory) = pod_totals(&item);
        assert!((cpu - 0.2).abs() < 1e-9);
        assert_eq!(memory, 256.0 * 1024.0 * 1024.0 + 1024.0f64.powi(3));
        assert_eq!(format_millicores(cpu), "200m");
        assert_eq!(format_mebibytes(memory), "1280Mi");
    }

    #[test]
    fn pod_totals_missing_containers_is_zero() {
        assert_eq!(pod_totals(&json!({})), (0.0, 0.0));
    }

    #[test]
    fn node_usage_parses() {
        let item = json!({"usage": {"cpu": "250m", "memory": "1Gi"}});
        assert_eq!(node_usage(&item), Some((0.25, 1024.0f64.powi(3))));
        assert_eq!(node_usage(&json!({})), None);
    }

    #[test]
    fn percent_rounds_or_dashes() {
        assert_eq!(percent(1.0, Some(4.0)), "25%");
        assert_eq!(percent(1.0, None), "-");
        assert_eq!(percent(1.0, Some(0.0)), "-");
    }

    #[test]
    fn allocatable_map_skips_incomplete_nodes() {
        let nodes = vec![
            json!({"metadata": {"name": "n1"},
                   "status": {"allocatable": {"cpu": "4", "memory": "16Gi"}}}),
            json!({"metadata": {"name": "n2"}}),
        ];
        let map = allocatable_map(&nodes);
        assert_eq!(map.len(), 1);
        assert_eq!(map["n1"], (4.0, 16.0 * 1024.0f64.powi(3)));
    }
}
