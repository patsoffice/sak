//! `sak k8s logs <pod>` — fetch container logs.
//!
//! Buffered, not streaming: kube exposes `log_stream` for true streaming, but
//! the rest of sak is sync-batch by design and the `--limit` and `--tail` flags
//! already bound the output. If a real need for streaming surfaces later, it
//! warrants its own follow-up issue rather than retrofitting an async pump
//! here.
//!
//! Output is the raw log bytes from the apiserver, line by line, optionally
//! prefixed with `[container] ` when `--all-containers` is set so the LLM can
//! tell sidecar output apart from the main container. All output flows through
//! [`crate::output::BoundedWriter`].

use std::io;
use std::process::ExitCode;

use anyhow::{Context, Result, anyhow};
use clap::Args;
use kube::api::LogParams;
use regex::Regex;
use serde_json::Value;

use crate::k8s::{client, discovery};
use crate::output::BoundedWriter;

#[derive(Args)]
#[command(
    about = "Fetch logs from a pod",
    long_about = "Fetch the buffered logs of one or more containers in a pod \
        and emit them line by line. With `--all-containers`, every container's \
        log stream is fetched and each line is prefixed with `[container] ` so \
        sidecar output is distinguishable from the main container.\n\n\
        `--since` accepts a small humantime-style duration: a sequence of \
        integer/unit pairs where the unit is one of `s`, `m`, `h`, `d` (e.g. \
        `30s`, `5m`, `2h30m`, `1d`). It maps to `LogParams::since_seconds`.\n\n\
        `--grep` filters lines client-side with a regex (the `regex` crate \
        flavour). Cheap, and saves you from having to pipe the output through \
        `sak fs grep` for every query.\n\n\
        Exit codes: 0 = at least one line emitted, 1 = pod not found or no \
        lines after filtering, 2 = error.",
    after_help = "\
Examples:
  sak k8s logs web-0 -n web                           Logs of the only / sole container
  sak k8s logs web-0 -n web --tail 50                 Last 50 lines
  sak k8s logs web-0 -n web --since 10m               Last 10 minutes
  sak k8s logs web-0 -n web --grep ERROR              Only ERROR lines
  sak k8s logs web-0 -n web -c app -p                 Previous instance of `app`
  sak k8s logs web-0 -n web --all-containers          Every container, prefixed"
)]
pub struct LogsArgs {
    /// Pod name
    pub pod: String,

    /// Container name (required when the pod has multiple containers unless
    /// `--all-containers` is set)
    #[arg(short, long, conflicts_with = "all_containers")]
    pub container: Option<String>,

    /// Emit logs from every container in the pod, prefixed with `[container] `
    #[arg(long)]
    pub all_containers: bool,

    /// Last N lines (maps to `LogParams::tail_lines`)
    #[arg(long)]
    pub tail: Option<i64>,

    /// Only logs newer than this duration (e.g. `30s`, `5m`, `2h30m`, `1d`)
    #[arg(long)]
    pub since: Option<String>,

    /// Read the previous container instance instead of the current one
    #[arg(short = 'p', long)]
    pub previous: bool,

    /// Line-oriented regex filter applied client-side
    #[arg(long)]
    pub grep: Option<String>,

    /// Namespace (default: current context's default namespace)
    #[arg(short, long)]
    pub namespace: Option<String>,

    /// Maximum number of output lines
    #[arg(long)]
    pub limit: Option<usize>,
}

pub async fn run(args: &LogsArgs) -> Result<ExitCode> {
    let client = client::build_client().await?;

    let ns = args
        .namespace
        .clone()
        .unwrap_or_else(|| client.default_namespace().to_string());

    let grep_re = match &args.grep {
        Some(p) => Some(Regex::new(p).with_context(|| format!("invalid --grep regex: {p}"))?),
        None => None,
    };

    let since_seconds = match &args.since {
        Some(s) => Some(parse_since(s)?),
        None => None,
    };

    // Decide which container(s) to fetch. If the user passed `--container`,
    // trust it without an extra GET. Otherwise we need the pod itself either
    // to enumerate containers (`--all-containers`) or to auto-pick the only
    // container (and to error helpfully when there's more than one).
    let containers: Vec<String> = if let Some(c) = &args.container {
        vec![c.clone()]
    } else {
        let (ar, _caps) = discovery::resolve(&client, "pod").await?;
        let obj = client::get_dyn(&client, &ar, Some(&ns), &args.pod).await?;
        let Some(obj) = obj else {
            return Ok(ExitCode::from(1));
        };
        let value: Value = serde_json::to_value(&obj)?;
        let names = container_names(&value);
        if names.is_empty() {
            return Err(anyhow!("pod {ns}/{} has no containers", args.pod));
        }
        if args.all_containers || names.len() == 1 {
            names
        } else {
            return Err(anyhow!(
                "pod {ns}/{} has multiple containers ({}); pass --container or --all-containers",
                args.pod,
                names.join(", ")
            ));
        }
    };

    let multi = containers.len() > 1;

    let stdout = io::stdout();
    let handle = stdout.lock();
    let mut writer = BoundedWriter::new(handle, args.limit);

    let mut wrote_any = false;
    'outer: for container in &containers {
        let lp = LogParams {
            container: Some(container.clone()),
            previous: args.previous,
            tail_lines: args.tail,
            since_seconds,
            ..LogParams::default()
        };

        let body = client::pod_logs(&client, &ns, &args.pod, &lp).await?;
        for line in body.lines() {
            if let Some(re) = &grep_re
                && !re.is_match(line)
            {
                continue;
            }
            let out = if multi {
                format!("[{container}] {line}")
            } else {
                line.to_string()
            };
            if !writer.write_line(&out)? {
                break 'outer;
            }
            wrote_any = true;
        }
    }

    writer.flush()?;
    if wrote_any {
        Ok(ExitCode::SUCCESS)
    } else {
        Ok(ExitCode::from(1))
    }
}

/// Pull `spec.containers[*].name` from a pod value. Pure helper so we can
/// unit-test the multi-container detection on hand-built fixtures.
fn container_names(pod: &Value) -> Vec<String> {
    pod.get("spec")
        .and_then(|s| s.get("containers"))
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|c| c.get("name").and_then(Value::as_str).map(String::from))
                .collect()
        })
        .unwrap_or_default()
}

/// Parse a humantime-style duration into seconds.
///
/// Accepts a sequence of integer/unit pairs where the unit is one of:
/// - `s` — seconds
/// - `m` — minutes
/// - `h` — hours
/// - `d` — days
///
/// Examples: `30s`, `5m`, `2h30m`, `1d12h`. Whitespace is not allowed.
/// We roll our own rather than pulling in `humantime` for what amounts to
/// four units — the parser is small enough to test exhaustively.
fn parse_since(s: &str) -> Result<i64> {
    if s.is_empty() {
        return Err(anyhow!("empty --since duration"));
    }
    let mut total: i64 = 0;
    let mut num: i64 = 0;
    let mut had_digit = false;
    let mut had_unit = false;
    for ch in s.chars() {
        if let Some(d) = ch.to_digit(10) {
            num = num
                .checked_mul(10)
                .and_then(|n| n.checked_add(d as i64))
                .ok_or_else(|| anyhow!("--since duration overflow: {s}"))?;
            had_digit = true;
        } else {
            if !had_digit {
                return Err(anyhow!(
                    "invalid --since duration `{s}`: unit without digits"
                ));
            }
            let mult: i64 = match ch {
                's' => 1,
                'm' => 60,
                'h' => 3600,
                'd' => 86400,
                _ => {
                    return Err(anyhow!(
                        "invalid --since unit `{ch}` in `{s}` (expected s/m/h/d)"
                    ));
                }
            };
            let segment = num
                .checked_mul(mult)
                .ok_or_else(|| anyhow!("--since duration overflow: {s}"))?;
            total = total
                .checked_add(segment)
                .ok_or_else(|| anyhow!("--since duration overflow: {s}"))?;
            num = 0;
            had_digit = false;
            had_unit = true;
        }
    }
    if had_digit {
        return Err(anyhow!(
            "invalid --since duration `{s}`: trailing digits without unit"
        ));
    }
    if !had_unit {
        return Err(anyhow!("invalid --since duration `{s}`"));
    }
    Ok(total)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parse_since_simple_units() {
        assert_eq!(parse_since("10s").unwrap(), 10);
        assert_eq!(parse_since("5m").unwrap(), 300);
        assert_eq!(parse_since("2h").unwrap(), 7200);
        assert_eq!(parse_since("1d").unwrap(), 86400);
    }

    #[test]
    fn parse_since_compound() {
        assert_eq!(parse_since("2h30m").unwrap(), 2 * 3600 + 30 * 60);
        assert_eq!(parse_since("1d12h").unwrap(), 86400 + 12 * 3600);
        assert_eq!(parse_since("1h30m45s").unwrap(), 3600 + 30 * 60 + 45);
    }

    #[test]
    fn parse_since_rejects_invalid() {
        assert!(parse_since("").is_err());
        assert!(parse_since("10").is_err()); // no unit
        assert!(parse_since("m").is_err()); // unit without digits
        assert!(parse_since("10x").is_err()); // unknown unit
        assert!(parse_since("10s5").is_err()); // trailing digits
        assert!(parse_since("1 0s").is_err()); // whitespace
    }

    #[test]
    fn container_names_extracts_in_order() {
        let pod = json!({
            "spec": {
                "containers": [
                    {"name": "app"},
                    {"name": "sidecar"},
                ]
            }
        });
        assert_eq!(container_names(&pod), vec!["app", "sidecar"]);
    }

    #[test]
    fn container_names_empty_when_missing() {
        let pod = json!({"metadata": {"name": "p"}});
        assert!(container_names(&pod).is_empty());
    }
}
