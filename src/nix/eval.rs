use std::io::{self, Write};
use std::process::ExitCode;

use anyhow::{Result, bail};
use clap::Args;

use crate::nix::client;
use crate::output::BoundedWriter;

#[derive(Args)]
#[command(
    about = "Evaluate a Nix expression / attribute (read-only)",
    long_about = "Evaluate a Nix value via `nix eval` and print it as JSON (default) or raw \
        text. The chokepoint always injects `--read-only`, so impure builtins \
        (`builtins.fetchurl`, import-from-derivation, ...) cannot trigger store \
        writes during evaluation.\n\n\
        There are three ways to name what to evaluate, and at least one is \
        required:\n  \
        • a positional INSTALLABLE — a flake attr / installable like \
        `.#packages.x86_64-linux.default.name` or `nixpkgs#hello.version` (a bare \
        positional is NOT a raw expression — nix treats it as a flake ref / path);\n  \
        • `--expr <EXPR>` — a raw Nix expression string (e.g. `--expr '1 + 2'`);\n  \
        • `--file <PATH>` (`-f`) — evaluate a `.nix` file.\n\n\
        `--apply <FN>` applies a function to the result (e.g. `--apply \
        builtins.length`). `--raw` prints a string result as bare bytes with no \
        quoting or trailing newline (handy for extracting a store path or version \
        and piping it on); without it the result is JSON. `--limit` bounds the \
        JSON line output; it does not apply to `--raw` (byte-faithful passthrough).\n\n\
        Exit status: 0 on a successful evaluation (a result of `null` / `false` / \
        `\"\"` still counts), 2 on an evaluation or tool error.",
    after_help = "\
Examples:
  sak nix eval --expr '1 + 2'                  Evaluate a raw expression -> 3
  sak nix eval .#packages.x86_64-linux.default.name   Flake attr as JSON
  sak nix eval nixpkgs#hello.version --raw     Bare version string
  sak nix eval --expr 'builtins.nixVersion' --raw
  sak nix eval --file ./values.nix             Evaluate a .nix file
  sak nix eval --expr '[1 2 3]' --apply builtins.length"
)]
pub struct EvalArgs {
    /// Installable / attr-path to evaluate (a flake ref, not a raw expression)
    #[arg(value_name = "INSTALLABLE")]
    pub installable: Option<String>,

    /// Evaluate a raw Nix expression string instead of an installable
    #[arg(long, value_name = "EXPR", conflicts_with = "file")]
    pub expr: Option<String>,

    /// Evaluate a `.nix` file
    #[arg(long, short = 'f', value_name = "PATH", conflicts_with = "expr")]
    pub file: Option<String>,

    /// Apply a function to the result (e.g. `builtins.length`)
    #[arg(long, value_name = "FN")]
    pub apply: Option<String>,

    /// Print a string result as bare bytes (no quotes, no trailing newline)
    #[arg(long)]
    pub raw: bool,

    /// Maximum number of output lines (JSON mode only; bounds output, not the eval)
    #[arg(long)]
    pub limit: Option<usize>,
}

pub fn run(args: &EvalArgs) -> Result<ExitCode> {
    if args.installable.is_none() && args.expr.is_none() && args.file.is_none() {
        bail!("nix eval needs an INSTALLABLE positional, `--expr <expr>`, or `--file <path>`");
    }

    let argv = build_argv(args);
    let argv_refs: Vec<&str> = argv.iter().map(String::as_str).collect();
    let stdout = client::invoke_ok("eval", None, &argv_refs)?;

    if args.raw {
        // Byte-faithful: bypass BoundedWriter so a `--raw` string (store path,
        // version, ...) round-trips unchanged. --limit is documented to apply
        // only in JSON mode. Mirrors `sak talos read`'s single-node path.
        io::stdout().write_all(&stdout)?;
        return Ok(ExitCode::SUCCESS);
    }

    // JSON mode: nix appends a trailing newline; re-emit line-wise so --limit
    // bounds large attrset / list output. A successful eval always produces a
    // value, so there is no exit-1 "no results" state.
    let out = io::stdout();
    let mut writer = BoundedWriter::new(out.lock(), args.limit);
    let text = String::from_utf8_lossy(&stdout);
    for line in text.split_inclusive('\n') {
        let line = line.strip_suffix('\n').unwrap_or(line);
        if !writer.write_line(line)? {
            break;
        }
    }
    writer.flush()?;
    Ok(ExitCode::SUCCESS)
}

/// Assemble the `nix eval` argv (the chokepoint injects `--read-only` and the
/// experimental-features flags). `--json` vs `--raw` selects the output
/// encoding; the source (installable / `--expr` / `--file`) and `--apply` ride
/// after it. The positional installable goes last. Pure so it's unit-testable.
fn build_argv(args: &EvalArgs) -> Vec<String> {
    let mut v = Vec::new();
    v.push(if args.raw { "--raw" } else { "--json" }.to_string());
    if let Some(expr) = &args.expr {
        v.push("--expr".to_string());
        v.push(expr.clone());
    }
    if let Some(file) = &args.file {
        v.push("--file".to_string());
        v.push(file.clone());
    }
    if let Some(apply) = &args.apply {
        v.push("--apply".to_string());
        v.push(apply.clone());
    }
    if let Some(inst) = &args.installable {
        v.push(inst.clone());
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bare() -> EvalArgs {
        EvalArgs {
            installable: None,
            expr: None,
            file: None,
            apply: None,
            raw: false,
            limit: None,
        }
    }

    #[test]
    fn json_is_default_encoding() {
        let mut a = bare();
        a.expr = Some("1 + 2".to_string());
        assert_eq!(build_argv(&a), vec!["--json", "--expr", "1 + 2"]);
    }

    #[test]
    fn raw_swaps_the_encoding_flag() {
        let mut a = bare();
        a.raw = true;
        a.installable = Some("nixpkgs#hello.version".to_string());
        assert_eq!(build_argv(&a), vec!["--raw", "nixpkgs#hello.version"]);
    }

    #[test]
    fn file_and_apply_thread_through_with_installable_last() {
        let mut a = bare();
        a.file = Some("./values.nix".to_string());
        a.apply = Some("builtins.length".to_string());
        a.installable = Some("foo".to_string());
        assert_eq!(
            build_argv(&a),
            vec![
                "--json",
                "--file",
                "./values.nix",
                "--apply",
                "builtins.length",
                "foo",
            ]
        );
    }

    #[test]
    fn run_errors_without_a_source() {
        let err = run(&bare()).unwrap_err();
        assert!(err.to_string().contains("needs an INSTALLABLE"));
    }
}
