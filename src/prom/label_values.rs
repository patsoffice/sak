//! `sak prom label-values <name>` — list values for one label.
//!
//! Queries `/api/v1/label/<name>/values` and emits one value per line,
//! sorted ascending. The natural follow-up to `sak prom labels`: once you
//! know a label name exists, this enumerates its observed values.

use std::io;
use std::process::ExitCode;

use anyhow::Result;
use clap::Args;

use crate::output::BoundedWriter;
use crate::prom::client::{PromClient, resolve_endpoint};
use crate::prom::labels::extract_strings;
use crate::prom::output::emit_json;
use crate::prom::query::urlencode;

#[derive(Args)]
#[command(
    about = "List values for one label",
    long_about = "List every observed value for the given label name from \
        `/api/v1/label/<name>/values`. One value per line, sorted ascending.\n\n\
        Pair with `sak prom labels` to first enumerate available label names. \
        Use `sak prom label-values namespace` on a Kubernetes-scraped Prom \
        for a quick namespace inventory.\n\n\
        Connection: pass --url <http://prom:9090> or set PROMETHEUS_URL.",
    after_help = "\
Examples:
  sak prom label-values namespace               Values of the `namespace` label
  sak prom label-values job                     Values of the `job` label
  sak prom label-values __name__                Every metric name on the server
  sak prom label-values job --json              Raw JSON for piping"
)]
pub struct LabelValuesArgs {
    /// The label name whose values to list (e.g. `job`, `namespace`)
    #[arg(value_name = "NAME")]
    pub name: String,

    /// Prometheus base URL (overrides PROMETHEUS_URL env)
    #[arg(long, value_name = "URL")]
    pub url: Option<String>,

    /// Emit the raw JSON response from /api/v1/label/<name>/values
    #[arg(long)]
    pub json: bool,

    /// Maximum number of output lines
    #[arg(long)]
    pub limit: Option<usize>,
}

pub fn run(args: &LabelValuesArgs) -> Result<ExitCode> {
    let endpoint = resolve_endpoint(args.url.as_deref(), "PROMETHEUS_URL")?;
    let client = PromClient::new(endpoint);
    let path = format!("/api/v1/label/{}/values", urlencode(&args.name));
    let data = match client.get_prom(&path)? {
        Some(v) => v,
        None => return Ok(ExitCode::from(1)),
    };

    if args.json {
        return emit_json(&data, args.limit);
    }

    let mut values = extract_strings(&data, &path)?;
    values.sort();

    let stdout = io::stdout();
    let handle = stdout.lock();
    let mut writer = BoundedWriter::new(handle, args.limit);

    let mut wrote_any = false;
    for v in &values {
        if !writer.write_line(v)? {
            break;
        }
        wrote_any = true;
    }
    writer.flush()?;
    Ok(if wrote_any {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    })
}
