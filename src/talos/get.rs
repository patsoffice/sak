use std::io;
use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::Result;
use clap::{Args, ValueEnum};

use crate::output::BoundedWriter;
use crate::talos::client;
use crate::talos::config;

#[derive(Copy, Clone, Debug, PartialEq, Eq, ValueEnum)]
pub enum Format {
    Yaml,
    Json,
}

impl Format {
    fn talosctl_value(self) -> &'static str {
        match self {
            Format::Yaml => "yaml",
            Format::Json => "json",
        }
    }
}

#[derive(Args)]
#[command(
    about = "Fetch a Talos COSI resource from one or more nodes",
    long_about = "Run `talosctl get <type> [name]` against each node in the \
        active talosconfig context and emit the result.\n\nDefaults to YAML \
        output (matching the talosctl default). Use --format json for JSON. \
        Multi-node output is separated by `---` document separators (YAML) or \
        emitted one JSON value per line (NDJSON-style); each section is \
        prefixed with a `# node=<ip>` comment so origin is clear.\n\nThe \
        `<type>` argument is the COSI resource type (`members`, \
        `kubernetesendpoints`, `mounts`, `services`, ...). Run `talosctl get \
        --help` to discover the full type list.\n\nDefault scope is every \
        node in the active context. Use `--node <ip>` for one node or \
        `--node <ip1,ip2>` for an explicit subset.\n\nThe verb passed to \
        `talosctl` is restricted to `get` by the chokepoint, so this is \
        strictly read-only.",
    after_help = "\
Examples:
  sak talos get members --node 192.168.1.10           etcd members from one node
  sak talos get services                              All nodes, YAML
  sak talos get services --format json                All nodes, JSON
  sak talos get kubernetesendpoints --node 192.168.1.10
  sak talos get mounts --node 192.168.1.10 | sak fs grep '/var'"
)]
pub struct GetArgs {
    /// COSI resource type (e.g. `members`, `services`, `mounts`)
    pub resource: String,

    /// Optional resource name to filter to one instance
    pub name: Option<String>,

    /// Talosconfig path (defaults to $TALOSCONFIG, then ~/.talos/config)
    #[arg(long)]
    pub talosconfig: Option<PathBuf>,

    /// Node IP(s): a single IP, a comma-separated list, or `all` (default)
    #[arg(long, value_name = "SPEC")]
    pub node: Option<String>,

    /// Output format (yaml or json; default yaml)
    #[arg(long, value_enum, default_value_t = Format::Yaml)]
    pub format: Format,

    /// Maximum number of output lines
    #[arg(long)]
    pub limit: Option<usize>,
}

pub fn run(args: &GetArgs) -> Result<ExitCode> {
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

    // Assemble the talosctl arg vector. `-o <fmt>` follows the resource so
    // the chokepoint doesn't need to know about output flags.
    let fmt = args.format.talosctl_value();
    let mut talos_args: Vec<&str> = vec![&args.resource];
    if let Some(name) = &args.name {
        talos_args.push(name);
    }
    talos_args.push("-o");
    talos_args.push(fmt);

    let stdout = io::stdout();
    let handle = stdout.lock();
    let mut writer = BoundedWriter::new(handle, args.limit);

    let mut any_success = false;
    for (i, node) in nodes.iter().enumerate() {
        let bytes = match client::invoke_ok("get", &talos_args, Some(node), Some(&cfg.path)) {
            Ok(b) => b,
            Err(e) => {
                writer.write_decoration(&format!("# node={} ERROR: {}", node, e))?;
                continue;
            }
        };
        any_success = true;
        if i > 0 {
            // YAML's document separator doubles as a visual gap for JSON;
            // it's only semantically meaningful in YAML, but rendering it
            // in both modes keeps the section header position consistent.
            writer.write_decoration("---")?;
        }
        writer.write_decoration(&format!("# node={}", node))?;
        for line in String::from_utf8_lossy(&bytes).split_inclusive('\n') {
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
