use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::Result;
use clap::Args;

use crate::cert::{CertInfo, FIELD_NAMES, OutputFormat, extract_ders, inspect, parse_cert};
use crate::talos::client;
use crate::talos::config;

#[derive(Args)]
#[command(
    about = "Inspect static-pod and kubelet certs across Talos nodes",
    long_about = "Read every well-known control-plane and kubelet certificate \
        off each node in the active talosconfig context and inspect it.\n\n\
        For each (node, cert path) pair, this runs `talosctl read <path>`, \
        pipes the result through the same auto-detect / parse logic as \
        `sak cert inspect`, and emits one record per cert. The `context` \
        column carries `node=<ip>:path=<path>` so per-node and per-role \
        filtering is a `grep`/`awk` away. Paths that don't exist on a given \
        node (e.g. kubelet cert on a control-plane-only node) are silently \
        skipped — Talos returns an error which we treat as \"not present.\"\n\n\
        Default scope is every node in the active context. Use `--node <ip>` \
        for a single node or `--node <ip1,ip2>` for an explicit subset.\n\n\
        This command is the killer use case for the cert/talos pairing: it \
        replaces a hand-rolled `for n in <ips>; do talosctl -n $n read … | \
        openssl x509 -noout -subject -dates; done` loop with a single, \
        structured command.",
    after_help = "\
Examples:
  sak talos certs                                     All certs, all nodes
  sak talos certs --node 192.168.1.10                 One node only
  sak talos certs --tsv                               TSV for spreadsheets
  sak talos certs --field not_after                   Just expiry dates
  sak talos certs --json | sak json query '.[].context'  Cert origins"
)]
pub struct CertsArgs {
    /// Talosconfig path (defaults to $TALOSCONFIG, then ~/.talos/config)
    #[arg(long)]
    pub talosconfig: Option<PathBuf>,

    /// Node IP(s): a single IP, a comma-separated list, or `all` (default)
    #[arg(long, value_name = "SPEC")]
    pub node: Option<String>,

    /// Output format (default: kv)
    #[arg(long, value_enum, default_value_t = OutputFormat::Kv, conflicts_with = "field")]
    pub format: OutputFormat,

    /// Convenience for --format json
    #[arg(long, conflicts_with_all = ["tsv", "field", "format"])]
    pub json: bool,

    /// Convenience for --format tsv
    #[arg(long, conflicts_with_all = ["json", "field", "format"])]
    pub tsv: bool,

    /// Print only this field, one value per cert
    #[arg(long, value_name = "NAME")]
    pub field: Option<String>,

    /// Maximum number of output lines
    #[arg(long)]
    pub limit: Option<usize>,
}

/// Talos cert paths we probe on every node. Compiled in rather than fetched
/// dynamically because the layout has been stable across Talos releases and
/// fetching the directory listing would be a second round trip per node.
///
/// Sources (verify when bumping Talos): the talos source tree under
/// `pkg/machinery/constants/constants.go` (look for `KubernetesPKIDir`,
/// `KubeletPKIDir`, etc.) and `talosctl read --help` examples.
const CERT_PATHS: &[&str] = &[
    // Control-plane node paths — silently absent on workers.
    "/system/secrets/kubernetes/kube-apiserver/apiserver.crt",
    "/system/secrets/kubernetes/kube-apiserver/apiserver-kubelet-client.crt",
    "/system/secrets/kubernetes/kube-apiserver/apiserver-etcd-client.crt",
    "/system/secrets/kubernetes/kube-apiserver/front-proxy-client.crt",
    "/system/secrets/kubernetes/etcd/server.crt",
    "/system/secrets/kubernetes/etcd/peer.crt",
    // Every node has a kubelet client cert — but its name varies, so the
    // current symlink is what we read.
    "/var/lib/kubelet/pki/kubelet-client-current.pem",
];

pub fn run(args: &CertsArgs) -> Result<ExitCode> {
    let format = if args.json {
        OutputFormat::Json
    } else if args.tsv {
        OutputFormat::Tsv
    } else {
        args.format
    };

    if let Some(field) = &args.field
        && !FIELD_NAMES.contains(&field.as_str())
    {
        anyhow::bail!(
            "unknown --field `{}` (valid: {})",
            field,
            FIELD_NAMES.join(", ")
        );
    }

    let cfg_path = config::resolve_path(args.talosconfig.as_deref())?;
    let cfg = config::load(&cfg_path)?;
    let nodes = config::resolve_nodes(&cfg, args.node.as_deref());

    if nodes.is_empty() {
        anyhow::bail!(
            "no nodes resolved from talosconfig `{}` (active context `{}`)",
            cfg.path.display(),
            cfg.context
        );
    }

    let mut infos: Vec<CertInfo> = Vec::new();
    for node in &nodes {
        for path in CERT_PATHS {
            // `talosctl read <missing-path>` exits non-zero. We treat that
            // as "this node doesn't have this file" and move on rather than
            // surfacing an error per missing path — it's the expected case
            // for worker nodes hitting control-plane-only paths.
            let bytes = match client::invoke_ok("read", &[path], Some(node), Some(&cfg.path)) {
                Ok(b) => b,
                Err(_) => continue,
            };
            let ders = match extract_ders(&bytes) {
                Ok(d) => d,
                Err(_) => continue,
            };
            for (i, der) in ders.iter().enumerate() {
                let context = format!("node={}:path={}", node, path);
                infos.push(parse_cert(node, i, der, &context)?);
            }
        }
    }

    if infos.is_empty() {
        return Ok(ExitCode::from(1));
    }

    // `inspect::emit` returns `Result<Outcome>` after the cert-domain phase-2
    // migration; bridge to ExitCode until phase 3 migrates the talos domain.
    inspect::emit(&infos, format, args.field.as_deref(), args.limit)
        .map(crate::output::Outcome::exit_code)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cert_paths_are_absolute() {
        for p in CERT_PATHS {
            assert!(p.starts_with('/'), "cert path must be absolute: {}", p);
        }
    }
}
