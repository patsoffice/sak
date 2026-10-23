//! `sak k8s endpoints <service>` — backing addresses of a Service.
//!
//! Reads `discovery.k8s.io/v1` EndpointSlices (the legacy `core/v1`
//! Endpoints object is deprecated) filtered by the
//! `kubernetes.io/service-name` label, then flattens one row per
//! (endpoint × port) combination: `address<TAB>port<TAB>node<TAB>ready`
//! `<TAB>target`, sorted by (address, port).

use crate::output::Outcome;
use std::io;

use anyhow::Result;
use clap::Args;
use kube::api::{ApiResource, ListParams};
use serde_json::Value;

use crate::k8s::client;
use crate::output::BoundedWriter;

#[derive(Args)]
#[command(
    about = "List backing endpoints of a Service",
    long_about = "List the backing endpoints of a Service — the fastest answer to \
        'Service has no endpoints'. Reads EndpointSlices labelled for the service \
        and prints one row per (endpoint × port) combination:\n\n\
        `address<TAB>port<TAB>node<TAB>ready<TAB>target`, sorted by (address, port). \
        `port` renders as `name=port/protocol` (`port/protocol` when the port is \
        unnamed); `node` is the endpoint's nodeName; `ready` is true/false/unknown; \
        `target` is the targetRef as `Kind/name`. Missing pieces render as `-`.\n\n\
        Zero EndpointSlices (or zero rows) is exit 1 — a service with no endpoints \
        is the diagnostic this command reports.",
    after_help = "\
Examples:
  sak k8s endpoints kubernetes -n default   Endpoints of the API server service
  sak k8s endpoints web -n prod             Backing pods of web in prod"
)]
pub struct EndpointsArgs {
    /// Service name
    pub service: String,

    /// Namespace scope (default: cluster default from kubeconfig)
    #[arg(short, long)]
    pub namespace: Option<String>,

    /// Maximum number of output lines
    #[arg(long)]
    pub limit: Option<usize>,
}

struct EndpointRow {
    address: String,
    port: String,
    node: String,
    ready: String,
    target: String,
}

/// Flatten EndpointSlice objects into rows. Pure over `serde_json::Value`
/// so it is unit-testable on hand-built fixtures with no cluster.
fn endpoint_rows(slices: &[Value]) -> Vec<EndpointRow> {
    let mut rows = Vec::new();
    for slice in slices {
        let ports: Vec<(Option<String>, Option<i64>, String)> = slice
            .get("ports")
            .and_then(Value::as_array)
            .map(|ps| {
                ps.iter()
                    .map(|p| {
                        (
                            p.get("name").and_then(Value::as_str).map(str::to_string),
                            p.get("port").and_then(Value::as_i64),
                            p.get("protocol")
                                .and_then(Value::as_str)
                                .unwrap_or("TCP")
                                .to_string(),
                        )
                    })
                    .collect()
            })
            .unwrap_or_default();
        let endpoints = slice
            .get("endpoints")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        for ep in &endpoints {
            let address = ep
                .get("addresses")
                .and_then(Value::as_array)
                .and_then(|a| a.first())
                .and_then(Value::as_str)
                .unwrap_or("-")
                .to_string();
            let node = ep
                .get("nodeName")
                .and_then(Value::as_str)
                .unwrap_or("-")
                .to_string();
            let ready = ep
                .get("conditions")
                .and_then(|c| c.get("ready"))
                .and_then(Value::as_bool)
                .map(|r| r.to_string())
                .unwrap_or_else(|| "unknown".to_string());
            let target = match (
                ep.get("targetRef")
                    .and_then(|t| t.get("kind"))
                    .and_then(Value::as_str),
                ep.get("targetRef")
                    .and_then(|t| t.get("name"))
                    .and_then(Value::as_str),
            ) {
                (Some(k), Some(n)) => format!("{k}/{n}"),
                (_, Some(n)) => n.to_string(),
                _ => "-".to_string(),
            };
            // A slice with no ports section still yields one row per
            // endpoint with a `-` port, so port-less slices are visible.
            if ports.is_empty() {
                rows.push(EndpointRow {
                    address: address.clone(),
                    port: "-".to_string(),
                    node: node.clone(),
                    ready: ready.clone(),
                    target: target.clone(),
                });
                continue;
            }
            for (name, port, proto) in &ports {
                let port = match (name, port) {
                    (Some(n), Some(p)) => format!("{n}={p}/{proto}"),
                    (Some(n), None) => format!("{n}=-/{proto}"),
                    (None, Some(p)) => format!("{p}/{proto}"),
                    (None, None) => format!("-/{proto}"),
                };
                rows.push(EndpointRow {
                    address: address.clone(),
                    port,
                    node: node.clone(),
                    ready: ready.clone(),
                    target: target.clone(),
                });
            }
        }
    }
    rows.sort_by(|a, b| a.address.cmp(&b.address).then_with(|| a.port.cmp(&b.port)));
    rows
}

pub async fn run(args: &EndpointsArgs) -> Result<Outcome> {
    let client = client::build_client().await?;

    // discovery.k8s.io/v1/EndpointSlice is stable: hardcode it instead of
    // spending a discovery round trip (the shape get.rs resolves dynamically).
    let ar = ApiResource {
        group: "discovery.k8s.io".to_string(),
        version: "v1".to_string(),
        api_version: "discovery.k8s.io/v1".to_string(),
        kind: "EndpointSlice".to_string(),
        plural: "endpointslices".to_string(),
    };
    let namespace = args
        .namespace
        .clone()
        .unwrap_or_else(|| client.default_namespace().to_string());
    let lp = ListParams::default().labels(&format!("kubernetes.io/service-name={}", args.service));
    let list = client::list_dyn(&client, &ar, Some(&namespace), &lp).await?;

    let slices: Vec<Value> = list
        .items
        .iter()
        .filter_map(|o| serde_json::to_value(o).ok())
        .collect();
    let rows = endpoint_rows(&slices);
    if rows.is_empty() {
        return Ok(Outcome::NotFound);
    }

    let stdout = io::stdout();
    let handle = stdout.lock();
    let mut writer = BoundedWriter::new(handle, args.limit);
    writer.write_decoration("ADDRESS\tPORT\tNODE\tREADY\tTARGET")?;
    for row in &rows {
        if !writer.write_line(&format!(
            "{}\t{}\t{}\t{}\t{}",
            row.address, row.port, row.node, row.ready, row.target
        ))? {
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

    fn fixture() -> Value {
        // Two endpoints × two ports.
        json!({
            "addressType": "IPv4",
            "ports": [
                {"name": "http", "port": 80, "protocol": "TCP"},
                {"name": "metrics", "port": 9090, "protocol": "TCP"},
            ],
            "endpoints": [
                {"addresses": ["10.0.0.2"],
                 "conditions": {"ready": true},
                 "nodeName": "n1",
                 "targetRef": {"kind": "Pod", "name": "web-0"}},
                {"addresses": ["10.0.0.1"],
                 "conditions": {"ready": false},
                 "targetRef": {"kind": "Pod", "name": "web-1"}},
            ],
        })
    }

    #[test]
    fn flattens_endpoint_port_combinations_sorted() {
        let rows = endpoint_rows(&[fixture()]);
        let rendered: Vec<String> = rows
            .iter()
            .map(|r| {
                format!(
                    "{}\t{}\t{}\t{}\t{}",
                    r.address, r.port, r.node, r.ready, r.target
                )
            })
            .collect();
        assert_eq!(
            rendered,
            vec![
                "10.0.0.1\thttp=80/TCP\t-\tfalse\tPod/web-1",
                "10.0.0.1\tmetrics=9090/TCP\t-\tfalse\tPod/web-1",
                "10.0.0.2\thttp=80/TCP\tn1\ttrue\tPod/web-0",
                "10.0.0.2\tmetrics=9090/TCP\tn1\ttrue\tPod/web-0",
            ]
        );
    }

    #[test]
    fn missing_pieces_render_dashes() {
        let slice = json!({
            "ports": [{"port": 80}],
            "endpoints": [{"addresses": []}],
        });
        let rows = endpoint_rows(&[slice]);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].address, "-");
        assert_eq!(rows[0].port, "80/TCP");
        assert_eq!(rows[0].node, "-");
        assert_eq!(rows[0].ready, "unknown");
        assert_eq!(rows[0].target, "-");
    }

    #[test]
    fn portless_slice_yields_dash_port_rows() {
        let slice = json!({
            "endpoints": [{"addresses": ["10.0.0.9"]}],
        });
        let rows = endpoint_rows(&[slice]);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].port, "-");
    }

    #[test]
    fn empty_input_yields_no_rows() {
        assert!(endpoint_rows(&[]).is_empty());
    }
}
