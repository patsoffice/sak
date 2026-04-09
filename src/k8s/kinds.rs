//! `sak k8s kinds` — list every group/version/kind the connected cluster
//! exposes.
//!
//! This is the smallest sanity check that the discovery pipeline works
//! end-to-end and the highest-utility command for "what's available on this
//! cluster" exploration. It is one of the few legitimate uses of full
//! discovery — the user is explicitly asking for everything.

use std::io;
use std::process::ExitCode;

use anyhow::Result;
use clap::Args;
use kube::discovery::Scope;

use crate::k8s::discovery;
use crate::output::BoundedWriter;

#[derive(Args)]
#[command(
    about = "List every kind exposed by the cluster",
    long_about = "List every group/version/kind the connected Kubernetes cluster exposes.\n\n\
        Output is sorted and tab-separated:\n\n  \
        group/version<TAB>kind<TAB>namespaced|cluster\n\n\
        The core API group is rendered as just `v1` (no leading slash). \
        Use this to discover what kinds are available before reaching for \
        `sak k8s get`.",
    after_help = "\
Examples:
  sak k8s kinds                            List every GVK on the cluster
  sak k8s kinds | head                     Just the first page
  sak k8s kinds --limit 50                 Cap the output at 50 lines"
)]
pub struct KindsArgs {
    /// Maximum number of output lines
    #[arg(long)]
    pub limit: Option<usize>,
}

pub async fn run(args: &KindsArgs) -> Result<ExitCode> {
    let client = crate::k8s::client::build_client().await?;
    let mut entries = discovery::discover_all(&client).await?;

    // Sort by (group/version, kind) for deterministic output.
    // `ApiResource::api_version` is already "group/version" for non-core
    // groups and just "version" (e.g. "v1") for the core group, matching
    // kubectl's display convention.
    entries.sort_by(|(a, _), (b, _)| (&a.api_version, &a.kind).cmp(&(&b.api_version, &b.kind)));

    let stdout = io::stdout();
    let handle = stdout.lock();
    let mut writer = BoundedWriter::new(handle, args.limit);

    let mut found_any = false;
    for (ar, caps) in &entries {
        let scope = match caps.scope {
            Scope::Namespaced => "namespaced",
            Scope::Cluster => "cluster",
        };
        let line = format!("{}\t{}\t{}", ar.api_version, ar.kind, scope);
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
