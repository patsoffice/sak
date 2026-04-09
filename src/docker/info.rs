//! `sak docker info` — show full metadata + runtime state for a single container.
//!
//! Issues a `GET /containers/<id-or-name>/json` against the discovered unix
//! socket via [`super::client::DockerClient::get_json`] and emits the resulting
//! metadata as pretty-printed JSON. Docker's container inspect endpoint
//! already returns config + state inline, so a single call returns everything
//! an LLM typically wants to inspect about a container.
//!
//! A 404 from the daemon (container not found) maps to exit code 1; any other
//! error is exit code 2 with a message on stderr.

use std::io;
use std::process::ExitCode;

use anyhow::Result;
use clap::Args;

use crate::docker::client::DockerClient;
use crate::output::BoundedWriter;

#[derive(Args)]
#[command(
    about = "Show full metadata and state for a Docker container",
    long_about = "Show the full metadata and runtime state for a single Docker \
        container, identified by name or (full or short) ID. Calls the Engine \
        API's container inspect endpoint, whose response already includes the \
        live `State` block (status, pid, exit code, health) inline with the \
        container `Config`, `HostConfig`, `NetworkSettings`, and `Mounts`.\n\n\
        Output is pretty-printed JSON. Pipe through `sak json query <path>` \
        to extract a specific field. Exit code 1 if the container does not \
        exist; exit code 2 on any other error.",
    after_help = "\
Examples:
  sak docker info web1                              Full container + state as JSON
  sak docker info abc123                            Look up by short ID
  sak docker info web1 | sak json query .State.Status
  sak docker info web1 | sak json keys .Config     Inspect just the config keys"
)]
pub struct InfoArgs {
    /// Container name or ID (full or short)
    pub name: String,

    /// Maximum number of output lines
    #[arg(long)]
    pub limit: Option<usize>,
}

pub async fn run(args: &InfoArgs) -> Result<ExitCode> {
    let client = DockerClient::connect()?;

    let path = format!("/containers/{}/json", args.name);

    let metadata = match client.get_json(&path).await? {
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
