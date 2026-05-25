use std::process::ExitCode;

use anyhow::{Result, bail};
use clap::Args;

use crate::helm::client::{self, Conn};
use crate::output::{BoundedWriter, collapse_ws};

/// Findings table columns.
const COLUMNS: [&str; 3] = ["severity", "path", "message"];

/// One `helm lint` finding: a `[SEVERITY] path: message` line.
#[derive(Debug, PartialEq, Eq)]
pub struct Finding {
    pub severity: String,
    pub path: String,
    pub message: String,
}

/// Parsed `N chart(s) linted, M chart(s) failed` summary.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub struct Summary {
    pub chart_count: u32,
    pub failed_count: u32,
}

#[derive(Args)]
#[command(
    about = "Lint a chart for structural / syntax issues as TSV (read-only)",
    long_about = "Validate a chart's structure and syntax via `helm lint <chart>` and emit one \
        TSV row per finding (`severity, path, message`) followed by a summary \
        line (`result, passed|failed, chart_count, error_count`). `helm lint` \
        only reads the chart — it never modifies it.\n\n\
        `--values` / `--set` lint against specific values (both repeatable); \
        `--strict` escalates warnings to failures; `--with-subcharts` recurses \
        into dependencies.\n\n\
        Exit status inverts sak's usual convention to match `helm lint`'s own \
        0=pass / 1=fail: exit 0 when the chart passes (INFO/WARNING findings \
        are still reported but don't fail unless `--strict`), exit 1 when it \
        fails (one or more charts failed), and exit 2 only if `helm` itself \
        errors. This makes `if sak helm lint ./chart; then ...` read naturally.",
    after_help = "\
Examples:
  sak helm lint ./mychart                       Lint a chart; exit 1 on failure
  sak helm lint ./mychart --strict              Treat warnings as failures
  sak helm lint ./mychart --values prod.yaml --set image.tag=1.2
  sak helm lint ./mychart --with-subcharts      Recurse into dependencies"
)]
pub struct LintArgs {
    /// Path to a chart directory or packaged `.tgz`
    #[arg(value_name = "CHART")]
    pub chart: String,

    /// Values overlay file to lint against (repeatable, `helm --values`)
    #[arg(long = "values", short = 'f', value_name = "FILE")]
    pub values: Vec<String>,

    /// Inline value override `key=value` (repeatable, `helm --set`)
    #[arg(long = "set", value_name = "KEY=VALUE")]
    pub set: Vec<String>,

    /// Treat warnings as failures (forwarded to `helm --strict`)
    #[arg(long)]
    pub strict: bool,

    /// Lint subcharts too (forwarded to `helm --with-subcharts`)
    #[arg(long)]
    pub with_subcharts: bool,

    /// Maximum number of finding rows (the header and summary always print)
    #[arg(long)]
    pub limit: Option<usize>,
}

pub fn run(args: &LintArgs) -> Result<ExitCode> {
    let argv = build_argv(args);
    let argv_refs: Vec<&str> = argv.iter().map(String::as_str).collect();
    // `helm lint` reads local chart files only — no cluster. Use `invoke` (not
    // `invoke_ok`): a lint *failure* is a non-zero exit we must map to sak's
    // exit 1, not treat as a tool error.
    let out = client::invoke("lint", None, &argv_refs, Conn::default())?;
    let stdout = String::from_utf8_lossy(&out.stdout);

    // The summary is on stdout when the chart passes, but on stderr (prefixed
    // `Error: `) when it fails — look in both. Its absence means `helm` itself
    // errored (bad flag, etc.) rather than producing a lint result.
    let Some(summary) = parse_summary(&stdout).or_else(|| parse_summary(&out.stderr)) else {
        let trimmed = out.stderr.trim();
        let suffix = if trimmed.is_empty() {
            String::new()
        } else {
            format!(": {}", trimmed)
        };
        bail!("helm lint failed{}", suffix);
    };

    let findings = parse_findings(&stdout);
    let passed = summary.failed_count == 0;

    let o = std::io::stdout();
    let mut writer = BoundedWriter::new(o.lock(), args.limit);
    writer.write_decoration(&COLUMNS.join("\t"))?;
    for f in &findings {
        let row = format!("{}\t{}\t{}", f.severity, f.path, f.message);
        if !writer.write_line(&row)? {
            break;
        }
    }
    writer.write_decoration(&format!(
        "result\t{}\t{}\t{}",
        if passed { "passed" } else { "failed" },
        summary.chart_count,
        summary.failed_count
    ))?;
    writer.flush()?;

    // Inverted convention: pass = exit 0, fail = exit 1.
    Ok(if passed {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    })
}

/// Assemble the `helm lint` argv: the chart (positional) then the value/flag
/// options. Connection flags come from `Conn` (none needed — lint is offline).
fn build_argv(args: &LintArgs) -> Vec<String> {
    let mut v = vec![args.chart.clone()];
    for f in &args.values {
        v.push("--values".to_string());
        v.push(f.clone());
    }
    for s in &args.set {
        v.push("--set".to_string());
        v.push(s.clone());
    }
    if args.strict {
        v.push("--strict".to_string());
    }
    if args.with_subcharts {
        v.push("--with-subcharts".to_string());
    }
    v
}

/// Parse `helm lint`'s `[SEVERITY] path: message` finding lines from stdout.
/// A finding may have an empty path (`[ERROR] : unable to load chart`), and a
/// tab-indented continuation line is appended to the preceding finding's
/// message. `==> Linting`, blank, and summary lines are ignored. Pure over its
/// input so it's testable on hand-built fixtures.
pub fn parse_findings(stdout: &str) -> Vec<Finding> {
    let mut findings: Vec<Finding> = Vec::new();
    for line in stdout.lines() {
        if let Some(rest) = line.strip_prefix('[')
            && let Some((severity, tail)) = rest.split_once("] ")
        {
            let (path, message) = match tail.split_once(": ") {
                Some((p, m)) => (p.trim(), m.trim()),
                None => ("", tail.trim()),
            };
            findings.push(Finding {
                severity: severity.trim().to_string(),
                path: collapse_ws(path),
                message: collapse_ws(message),
            });
            continue;
        }
        // A non-empty line starting with whitespace continues the previous
        // finding's message (e.g. the `\tvalidation: ...` under an error).
        if !line.trim().is_empty()
            && line.starts_with(char::is_whitespace)
            && let Some(last) = findings.last_mut()
        {
            if !last.message.is_empty() {
                last.message.push(' ');
            }
            last.message.push_str(&collapse_ws(line.trim()));
        }
    }
    findings
}

/// Find the `N chart(s) linted, M chart(s) failed` summary in `text`, tolerating
/// a leading `Error: ` (helm prefixes it on stderr when a chart fails).
pub fn parse_summary(text: &str) -> Option<Summary> {
    for line in text.lines() {
        let l = line.trim();
        let l = l.strip_prefix("Error: ").unwrap_or(l);
        if let Some((counts, _)) = l.split_once(" chart(s) failed")
            && let Some((linted, failed)) = counts.split_once(" chart(s) linted, ")
            && let (Ok(chart_count), Ok(failed_count)) =
                (linted.trim().parse(), failed.trim().parse())
        {
            return Some(Summary {
                chart_count,
                failed_count,
            });
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    // Real `helm lint` stdout for a passing chart.
    const GOOD: &str = "==> Linting /tmp/ok\n\
[INFO] Chart.yaml: icon is recommended\n\
\n\
1 chart(s) linted, 0 chart(s) failed\n";

    // Real `helm lint` stdout for a failing chart (the summary lands on stderr,
    // see GOOD vs the `Error: ` form tested separately).
    const BAD: &str = "==> Linting /tmp/bad\n\
[ERROR] Chart.yaml: name is required\n\
[INFO] Chart.yaml: icon is recommended\n\
[INFO] values.yaml: file does not exist\n\
[ERROR] templates/: validation: chart.metadata.name is required\n\
[ERROR] : unable to load chart\n\
\tvalidation: chart.metadata.name is required\n\
\n";

    #[test]
    fn parse_good_finding_and_summary() {
        let findings = parse_findings(GOOD);
        assert_eq!(findings.len(), 1);
        assert_eq!(
            findings[0],
            Finding {
                severity: "INFO".into(),
                path: "Chart.yaml".into(),
                message: "icon is recommended".into(),
            }
        );
        assert_eq!(
            parse_summary(GOOD),
            Some(Summary {
                chart_count: 1,
                failed_count: 0
            })
        );
    }

    #[test]
    fn parse_bad_findings_count_and_severities() {
        let findings = parse_findings(BAD);
        assert_eq!(findings.len(), 5);
        assert_eq!(findings[0].severity, "ERROR");
        assert_eq!(findings[0].path, "Chart.yaml");
        assert_eq!(findings[0].message, "name is required");
    }

    #[test]
    fn parse_keeps_internal_colon_in_message() {
        let findings = parse_findings(BAD);
        assert_eq!(findings[3].path, "templates/");
        assert_eq!(
            findings[3].message,
            "validation: chart.metadata.name is required"
        );
    }

    #[test]
    fn parse_handles_empty_path_and_continuation() {
        let findings = parse_findings(BAD);
        let last = &findings[4];
        assert_eq!(last.severity, "ERROR");
        assert_eq!(last.path, "");
        // The tab-indented continuation line is appended to the message.
        assert_eq!(
            last.message,
            "unable to load chart validation: chart.metadata.name is required"
        );
    }

    #[test]
    fn parse_summary_tolerates_error_prefix() {
        // The failing-chart summary helm writes to stderr.
        assert_eq!(
            parse_summary("Error: 1 chart(s) linted, 1 chart(s) failed"),
            Some(Summary {
                chart_count: 1,
                failed_count: 1
            })
        );
    }

    #[test]
    fn parse_summary_absent_is_none() {
        assert_eq!(parse_summary("Error: open ./nope: no such file"), None);
        assert_eq!(parse_summary(""), None);
    }

    #[test]
    fn build_argv_forwards_flags() {
        let args = LintArgs {
            chart: "./c".into(),
            values: vec!["v.yaml".into()],
            set: vec!["a=b".into()],
            strict: true,
            with_subcharts: true,
            limit: None,
        };
        assert_eq!(
            build_argv(&args),
            vec![
                "./c",
                "--values",
                "v.yaml",
                "--set",
                "a=b",
                "--strict",
                "--with-subcharts",
            ]
        );
    }
}
