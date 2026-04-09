//! `sak lxc info` — show full metadata + runtime state for a single instance.
//!
//! Issues a `GET /1.0/instances/<name>?recursion=1` against the discovered
//! unix socket via [`super::client::LxcClient::get_json_recursive`] and emits
//! the resulting metadata as pretty-printed JSON. Recursion 1 inlines the
//! `state` (network, processes, memory, ...) so a single call returns
//! everything an LLM typically wants to inspect about an instance.
//!
//! A 404 from the daemon (instance not found) maps to exit code 1; any other
//! error is exit code 2 with a message on stderr.

use std::io;
use std::process::ExitCode;

use anyhow::Result;
use clap::Args;

use crate::lxc::client::LxcClient;
use crate::output::BoundedWriter;

#[derive(Args)]
#[command(
    about = "Show full metadata and state for an LXD/Incus instance",
    long_about = "Show the full metadata and runtime state for a single LXD or \
        Incus instance. Issues a recursion=1 query so the response includes \
        the live `state` block (network addresses, process counts, memory, \
        cpu) inline with the instance config.\n\n\
        Output is pretty-printed JSON. Pipe through `sak json query <path>` \
        to extract a specific field. Exit code 1 if the instance does not \
        exist; exit code 2 on any other error.",
    after_help = "\
Examples:
  sak lxc info web1                              Full instance + state as JSON
  sak lxc info web1 --project mylab              Look up in a specific project
  sak lxc info web1 | sak json query .state.network.eth0.addresses
  sak lxc info web1 | sak json keys .config      Inspect just the config keys"
)]
pub struct InfoArgs {
    /// Instance name
    pub name: String,

    /// LXD project to look the instance up in (default: `default`)
    #[arg(long)]
    pub project: Option<String>,

    /// Maximum number of output lines
    #[arg(long)]
    pub limit: Option<usize>,
}

pub async fn run(args: &InfoArgs) -> Result<ExitCode> {
    let client = LxcClient::connect()?;

    let path = match &args.project {
        Some(p) => format!("/1.0/instances/{}?project={p}", args.name),
        None => format!("/1.0/instances/{}", args.name),
    };

    let metadata = match client.get_json_recursive(&path, 1).await? {
        Some(v) => v,
        None => return Ok(ExitCode::from(1)),
    };

    let pretty = serde_json::to_string_pretty(&metadata)?;

    let stdout = io::stdout();
    let handle = stdout.lock();
    let mut writer = BoundedWriter::new(handle, args.limit);
    for line in pretty.lines() {
        if !writer.write_line(line)? {
            break;
        }
    }
    writer.flush()?;

    Ok(ExitCode::SUCCESS)
}
