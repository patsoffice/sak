//! `sak prom label-values <name>` — list values for one label.
//!
//! Queries `/api/v1/label/<name>/values` and emits one value per line,
//! sorted ascending. The natural follow-up to `sak prom labels`: once you
//! know a label name exists, this enumerates its observed values.

use crate::output::Outcome;

use anyhow::Result;
use clap::Args;

use crate::prom::common_args::CommonPromArgs;
use crate::prom::labels::extract_strings;
use crate::prom::query::urlencode;
use crate::prom::runner::run_prom;

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
    #[command(flatten)]
    pub common: CommonPromArgs,

    /// The label name whose values to list (e.g. `job`, `namespace`)
    #[arg(value_name = "NAME")]
    pub name: String,
}

pub fn run(args: &LabelValuesArgs) -> Result<Outcome> {
    let path = format!("/api/v1/label/{}/values", urlencode(&args.name));
    run_prom(&args.common, &path, |data| {
        let mut values = extract_strings(data, &path)?;
        values.sort();
        Ok(values)
    })
}
