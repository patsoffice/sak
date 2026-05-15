use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::Result;
use clap::Args;

use crate::json::read_json_inputs;
use crate::value::resolve_expression;

#[derive(Args)]
#[command(
    about = "Check whether a path exists in JSON",
    long_about = "Boolean check for whether a path exists in a JSON document.\n\n\
        Produces no stdout output — the result is signalled by the exit code: \
        0 if the path exists, 1 if it is missing. Useful in agent loops and \
        shell pipelines where the value itself isn't needed.\n\n\
        The expression accepts the same dot notation (e.g. `.users[0].name`) \
        and JSON Pointer syntax (e.g. `/users/0/name`) as `sak json query`. \
        Reads from stdin if no files are given.\n\n\
        With multiple files, the default is *all*: exit 0 only if every input \
        contains the path. Pass `--any` to flip the semantics (exit 0 if any \
        input contains the path).",
    after_help = "\
Examples:
  sak json exists .features.beta config.json && enable_beta
  echo '{\"a\":1}' | sak json exists .a            Exit code: 0
  echo '{\"a\":1}' | sak json exists .b            Exit code: 1
  sak json exists .api.key dev.json prod.json     All files must contain .api.key
  sak json exists --any .deprecated *.json        Any file containing .deprecated"
)]
pub struct ExistsArgs {
    /// Path expression (dot notation or JSON Pointer)
    pub expression: String,

    /// Input files (reads stdin if omitted)
    pub files: Vec<PathBuf>,

    /// Exit 0 if the path exists in *any* input (default: requires *all*)
    #[arg(long)]
    pub any: bool,
}

pub fn run(args: &ExistsArgs) -> Result<ExitCode> {
    let inputs = read_json_inputs(&args.files)?;

    let mut all_present = true;
    let mut any_present = false;
    for (_name, value) in &inputs {
        if resolve_expression(value, &args.expression)?.is_some() {
            any_present = true;
        } else {
            all_present = false;
        }
    }

    let exists = if args.any { any_present } else { all_present };
    if exists {
        Ok(ExitCode::SUCCESS)
    } else {
        Ok(ExitCode::from(1))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_tmp(content: &str) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.json");
        let mut f = std::fs::File::create(&p).unwrap();
        f.write_all(content.as_bytes()).unwrap();
        (dir, p)
    }

    #[test]
    fn exists_present() {
        let (_d, p) = write_tmp(r#"{"name":"alice"}"#);
        let args = ExistsArgs {
            expression: ".name".to_string(),
            files: vec![p],
            any: false,
        };
        assert_eq!(run(&args).unwrap(), ExitCode::SUCCESS);
    }

    #[test]
    fn exists_missing_returns_1() {
        let (_d, p) = write_tmp(r#"{"name":"alice"}"#);
        let args = ExistsArgs {
            expression: ".missing".to_string(),
            files: vec![p],
            any: false,
        };
        assert_eq!(run(&args).unwrap(), ExitCode::from(1));
    }

    #[test]
    fn exists_json_pointer() {
        let (_d, p) = write_tmp(r#"{"a":{"b":1}}"#);
        let args = ExistsArgs {
            expression: "/a/b".to_string(),
            files: vec![p],
            any: false,
        };
        assert_eq!(run(&args).unwrap(), ExitCode::SUCCESS);
    }

    #[test]
    fn exists_root_pointer() {
        let (_d, p) = write_tmp(r#"{"a":1}"#);
        let args = ExistsArgs {
            expression: "".to_string(),
            files: vec![p],
            any: false,
        };
        assert_eq!(run(&args).unwrap(), ExitCode::SUCCESS);
    }

    #[test]
    fn exists_null_value_still_counts_as_present() {
        let (_d, p) = write_tmp(r#"{"a":null}"#);
        let args = ExistsArgs {
            expression: ".a".to_string(),
            files: vec![p],
            any: false,
        };
        assert_eq!(run(&args).unwrap(), ExitCode::SUCCESS);
    }

    #[test]
    fn exists_all_requires_every_file() {
        let (_d1, p1) = write_tmp(r#"{"a":1}"#);
        let dir2 = tempfile::tempdir().unwrap();
        let p2 = dir2.path().join("b.json");
        std::fs::File::create(&p2)
            .unwrap()
            .write_all(br#"{"b":2}"#)
            .unwrap();
        let args = ExistsArgs {
            expression: ".a".to_string(),
            files: vec![p1, p2],
            any: false,
        };
        assert_eq!(run(&args).unwrap(), ExitCode::from(1));
    }

    #[test]
    fn exists_any_only_requires_one_file() {
        let (_d1, p1) = write_tmp(r#"{"a":1}"#);
        let dir2 = tempfile::tempdir().unwrap();
        let p2 = dir2.path().join("b.json");
        std::fs::File::create(&p2)
            .unwrap()
            .write_all(br#"{"b":2}"#)
            .unwrap();
        let args = ExistsArgs {
            expression: ".a".to_string(),
            files: vec![p1, p2],
            any: true,
        };
        assert_eq!(run(&args).unwrap(), ExitCode::SUCCESS);
    }
}
