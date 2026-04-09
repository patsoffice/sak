//! `sak k8s contexts` — list every context in the merged kubeconfig.
//!
//! This is the safety-first command of the k8s domain: it tells an LLM (and
//! the user double-checking the LLM) which kubeconfig context is currently
//! active *before* any read against a live cluster. It is the cheapest k8s
//! command in sak — it never contacts an apiserver. All it does is parse
//! kubeconfig (honoring `KUBECONFIG` and the standard search path the same
//! way `kube::Client::try_default` does) and pretty-print the result.
//!
//! Because it does not import `kube::Api`, the read-only chokepoint test in
//! `client.rs` is unaffected.

use std::io;
use std::process::ExitCode;

use anyhow::{Context, Result};
use clap::Args;
use kube::config::Kubeconfig;

use crate::output::BoundedWriter;

#[derive(Args)]
#[command(
    about = "List every context in the merged kubeconfig",
    long_about = "List every context in the merged kubeconfig and mark the active one.\n\n\
        This is the safest k8s command sak ships: it never contacts any \
        apiserver. It only parses kubeconfig (honoring `KUBECONFIG` and the \
        standard search path the same way `kubectl` does) and prints what it \
        finds. Run it first in any new k8s session to verify which cluster \
        the next `sak k8s` command is going to hit.\n\n\
        Output is sorted by context name and tab-separated:\n\n  \
        current<TAB>name<TAB>cluster<TAB>user<TAB>namespace\n\n\
        The `current` column is `*` for the active context and empty \
        otherwise. The `namespace` column is whatever kubeconfig literally \
        says — kube clients map an unset namespace to `default` at request \
        time, but sak reports the raw value so you can see when a context \
        is implicit.",
    after_help = "\
Examples:
  sak k8s contexts                          List every context, mark the active one
  sak k8s contexts --limit 20               Cap the output at 20 lines"
)]
pub struct ContextsArgs {
    /// Maximum number of output lines
    #[arg(long)]
    pub limit: Option<usize>,
}

pub async fn run(args: &ContextsArgs) -> Result<ExitCode> {
    let kubeconfig = Kubeconfig::read().context("failed to read kubeconfig")?;
    let current = kubeconfig.current_context.as_deref().unwrap_or("");

    let mut entries = kubeconfig.contexts;
    entries.sort_by(|a, b| a.name.cmp(&b.name));

    let stdout = io::stdout();
    let handle = stdout.lock();
    let mut writer = BoundedWriter::new(handle, args.limit);

    let mut found_any = false;
    for nc in &entries {
        let marker = if nc.name == current { "*" } else { "" };
        let (cluster, user, namespace) = match &nc.context {
            Some(c) => (
                c.cluster.as_str(),
                c.user.as_deref().unwrap_or(""),
                c.namespace.as_deref().unwrap_or(""),
            ),
            None => ("", "", ""),
        };
        let line = format!(
            "{}\t{}\t{}\t{}\t{}",
            marker, nc.name, cluster, user, namespace
        );
        if !writer.write_line(&line)? {
            writer.flush()?;
            return Ok(ExitCode::SUCCESS);
        }
        found_any = true;
    }

    writer.flush()?;
    if found_any {
        Ok(ExitCode::SUCCESS)
    } else {
        Ok(ExitCode::from(1))
    }
}
