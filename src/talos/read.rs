use std::io::{self, Write};
use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::Result;
use clap::Args;

use crate::output::BoundedWriter;
use crate::talos::client;
use crate::talos::config;

#[derive(Args)]
#[command(
    about = "Read a file from one or more Talos nodes",
    long_about = "Run `talosctl read <path>` against each node in the active \
        talosconfig context and emit the contents.\n\nFor a single node, \
        the file's bytes are written verbatim to stdout — handy for piping \
        binary content (e.g. into `sak cert inspect`). For multi-node fan-out, \
        each node's output is preceded by a `### node=<ip>` header line, \
        which makes the stream readable but not byte-faithful — use \
        `--node <ip>` for byte-faithful single-node mode.\n\n\
        Default scope is every node in the active context. Use `--node <ip>` \
        for one node or `--node <ip1,ip2>` for an explicit subset.\n\n\
        This command is the per-file equivalent of `sak talos certs`: it \
        replaces the `for n in <ips>; do talosctl -n $n read <path>; done` \
        loop. The verb passed to `talosctl` is restricted to `read` by the \
        chokepoint, so this is a strictly read-only wrapper.",
    after_help = "\
Examples:
  sak talos read /etc/os-release                              All nodes (decorated)
  sak talos read /etc/os-release --node 192.168.1.10          One node, raw bytes
  sak talos read /system/secrets/kubernetes/kube-apiserver/apiserver.crt \\
       --node 192.168.1.10 | sak cert inspect                Pipe a cert in
  sak talos read /etc/machine-id --node 192.168.1.10,192.168.1.11"
)]
pub struct ReadArgs {
    /// File path on the node
    pub path: String,

    /// Talosconfig path (defaults to $TALOSCONFIG, then ~/.talos/config)
    #[arg(long)]
    pub talosconfig: Option<PathBuf>,

    /// Node IP(s): a single IP, a comma-separated list, or `all` (default)
    #[arg(long, value_name = "SPEC")]
    pub node: Option<String>,

    /// Maximum number of output lines (only applies in multi-node mode)
    #[arg(long)]
    pub limit: Option<usize>,
}

pub fn run(args: &ReadArgs) -> Result<ExitCode> {
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

    if nodes.len() == 1 {
        // Byte-faithful single-node mode. We bypass BoundedWriter so binary
        // content (e.g. DER certs piped into `sak cert inspect`) round-trips
        // unchanged. --limit is documented to apply only in multi-node mode.
        let bytes = client::invoke_ok("read", &[&args.path], Some(&nodes[0]), Some(&cfg.path))?;
        io::stdout().write_all(&bytes)?;
        return Ok(ExitCode::SUCCESS);
    }

    // Multi-node mode: per-node section headers, lossy UTF-8 conversion so
    // binary files don't garble the output. Users who need raw bytes from a
    // multi-node fan-out should script around the per-node single-node form.
    let stdout = io::stdout();
    let handle = stdout.lock();
    let mut writer = BoundedWriter::new(handle, args.limit);

    let mut any_success = false;
    for (i, node) in nodes.iter().enumerate() {
        if i > 0 {
            writer.write_decoration("")?;
        }
        let bytes = match client::invoke_ok("read", &[&args.path], Some(node), Some(&cfg.path)) {
            Ok(b) => b,
            Err(e) => {
                writer.write_decoration(&format!("### node={} ERROR: {}", node, e))?;
                continue;
            }
        };
        any_success = true;
        writer.write_decoration(&format!("### node={}", node))?;
        for line in String::from_utf8_lossy(&bytes).split_inclusive('\n') {
            // split_inclusive keeps the trailing '\n'; write_line re-adds
            // one if missing, so trim it here to avoid doubled newlines.
            let trimmed = line.strip_suffix('\n').unwrap_or(line);
            if !writer.write_line(trimmed)? {
                writer.flush()?;
                return Ok(ExitCode::SUCCESS);
            }
        }
    }

    writer.flush()?;
    if any_success {
        Ok(ExitCode::SUCCESS)
    } else {
        Ok(ExitCode::from(1))
    }
}
