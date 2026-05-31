//! `sak loki label-values <name>` — list values for one label.
//!
//! Queries `/loki/api/v1/label/<name>/values` and emits one value per line,
//! sorted ascending. The natural follow-up to `sak loki labels`: once you
//! know a label name exists, this enumerates its observed values.

use crate::output::Outcome;

use anyhow::Result;
use clap::Args;

use crate::loki::common_args::CommonLokiArgs;
use crate::loki::labels::extract_strings;
use crate::loki::query::urlencode;
use crate::loki::runner::run_loki;

#[derive(Args)]
#[command(
    about = "List values for one label",
    long_about = "List every observed value for the given label name from \
        `/loki/api/v1/label/<name>/values`. One value per line, sorted \
        ascending.\n\n\
        Pair with `sak loki labels` to first enumerate available label names. \
        Use `sak loki label-values app` for a quick application inventory, or \
        `sak loki label-values namespace` on a Kubernetes-scraped Loki.\n\n\
        Connection: pass --url <http://loki:3100> or set LOKI_URL.",
    after_help = "\
Examples:
  sak loki label-values app                     Values of the `app` label
  sak loki label-values namespace               Values of the `namespace` label
  sak loki label-values level                   Values of the `level` label
  sak loki label-values app --json              Raw JSON for piping"
)]
pub struct LabelValuesArgs {
    #[command(flatten)]
    pub common: CommonLokiArgs,

    /// The label name whose values to list (e.g. `app`, `namespace`)
    #[arg(value_name = "NAME")]
    pub name: String,
}

pub fn run(args: &LabelValuesArgs) -> Result<Outcome> {
    let path = format!("/loki/api/v1/label/{}/values", urlencode(&args.name));
    run_loki(&args.common, &path, |data| {
        let mut values = extract_strings(data, &path)?;
        values.sort();
        Ok(values)
    })
}
