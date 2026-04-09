//! `sak k8s images [kind]` — list every container image running on the
//! cluster, one container per line.
//!
//! Walks resources of the given kind (default `pods`) via the shared
//! [`super::containers::walk_containers`] walker, sorts by
//! `(namespace, name, container)`, and emits
//! `namespace/name<TAB>container<TAB>image` through `BoundedWriter`.

use std::io;
use std::process::ExitCode;

use anyhow::{Result, bail};
use clap::Args;
use kube::api::ListParams;
use kube::discovery::Scope;
use serde_json::Value;

use crate::k8s::{client, containers, discovery};
use crate::output::BoundedWriter;

#[derive(Args)]
#[command(
    about = "List container images across resources",
    long_about = "List the container images running across resources of a kind.\n\n\
        Defaults to walking pods. Pass a kind (`deployment`, `statefulset`, \
        `cronjob`, ...) to walk pod-template owners instead — useful when you \
        want \"what's declared\" rather than \"what's currently scheduled.\"\n\n\
        Output is `namespace/name<TAB>container<TAB>image`, sorted by \
        (namespace, name, container).\n\n\
        Cluster-scoped kinds (e.g. nothing useful here today) are rejected if \
        --namespace or --all-namespaces is supplied.",
    after_help = "\
Examples:
  sak k8s images                            Pods in the current namespace
  sak k8s images -A                         Pods across every namespace
  sak k8s images deployment -A              Walk Deployments instead of Pods
  sak k8s images cronjob -n batch           CronJob containers in the batch ns
  sak k8s images -l app=nginx               Filter by label selector"
)]
pub struct ImagesArgs {
    /// Resource kind to walk (default: pods)
    #[arg(default_value = "pods")]
    pub kind: String,

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

pub async fn run(args: &ImagesArgs) -> Result<ExitCode> {
    let client = client::build_client().await?;
    let (ar, caps) = discovery::resolve(&client, &args.kind).await?;

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

    let mut lp = ListParams::default();
    if let Some(sel) = &args.selector {
        lp = lp.labels(sel);
    }

    let list = client::list_dyn(&client, &ar, effective_ns.as_deref(), &lp).await?;

    // Materialize all rows up front so we can sort. The total volume here is
    // bounded by `containers across resources`, which is much smaller than the
    // raw resource JSON we already loaded.
    let mut rows: Vec<(String, String, String, String)> = Vec::new();
    for obj in &list.items {
        let value: Value = serde_json::to_value(obj)?;
        for view in containers::walk_containers(&value) {
            rows.push((
                view.namespace.unwrap_or("").to_string(),
                view.name.to_string(),
                view.container.to_string(),
                view.image.to_string(),
            ));
        }
    }
    rows.sort();

    let stdout = io::stdout();
    let handle = stdout.lock();
    let mut writer = BoundedWriter::new(handle, args.limit);

    let mut wrote_any = false;
    for (ns, name, container, image) in &rows {
        let nsname = if ns.is_empty() {
            name.clone()
        } else {
            format!("{}/{}", ns, name)
        };
        let line = format!("{}\t{}\t{}", nsname, container, image);
        if !writer.write_line(&line)? {
            break;
        }
        wrote_any = true;
    }

    writer.flush()?;
    if wrote_any {
        Ok(ExitCode::SUCCESS)
    } else {
        Ok(ExitCode::from(1))
    }
}
