use std::io;
use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::Result;
use clap::Args;

use crate::json::read_json_inputs;
use crate::output::BoundedWriter;
use crate::value::{resolve_expression, type_name};

#[derive(Args)]
#[command(
    about = "Print the JSON type at a path",
    long_about = "Print the JSON type (object, array, string, number, boolean, null) \
        at a given path.\n\n\
        With no path, prints the type of the root value. Cheap discovery without \
        dumping the value itself — handy for branching agent logic on shape before \
        committing to a full extraction. Exits 1 if the path does not resolve. \
        Reads from stdin if no files are given, or for a file argument of `-`.",
    after_help = "\
Examples:
  echo '{\"name\":\"alice\"}' | sak json type
  sak json type data.json                          Type of the root
  sak json type .users data.json                   Type at a path
  sak json type /users/0 data.json                 JSON Pointer"
)]
pub struct TypeArgs {
    /// Path within the document (default: root)
    pub path: Option<String>,

    /// Input files (reads stdin if omitted or given as "-")
    pub files: Vec<PathBuf>,

    /// Maximum number of output lines
    #[arg(long)]
    pub limit: Option<usize>,
}

pub fn run(args: &TypeArgs) -> Result<ExitCode> {
    let inputs = read_json_inputs(&args.files)?;

    let stdout = io::stdout();
    let handle = stdout.lock();
    let mut writer = BoundedWriter::new(handle, args.limit);

    let mut found_any = false;
    for (_name, value) in &inputs {
        let target = match &args.path {
            Some(p) => match resolve_expression(value, p)? {
                Some(v) => v,
                None => continue,
            },
            None => value,
        };
        found_any = true;
        if !writer.write_line(type_name(target))? {
            writer.flush()?;
            return Ok(ExitCode::SUCCESS);
        }
    }

    writer.flush()?;
    if found_any {
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
    fn type_of_root_object() {
        let (_d, p) = write_tmp(r#"{"a":1}"#);
        let args = TypeArgs {
            path: None,
            files: vec![p],
            limit: None,
        };
        assert_eq!(run(&args).unwrap(), ExitCode::SUCCESS);
    }

    #[test]
    fn type_at_path() {
        let (_d, p) = write_tmp(r#"{"users":[1,2,3]}"#);
        let args = TypeArgs {
            path: Some(".users".to_string()),
            files: vec![p],
            limit: None,
        };
        assert_eq!(run(&args).unwrap(), ExitCode::SUCCESS);
    }

    #[test]
    fn type_missing_returns_1() {
        let (_d, p) = write_tmp(r#"{"a":1}"#);
        let args = TypeArgs {
            path: Some(".missing".to_string()),
            files: vec![p],
            limit: None,
        };
        assert_eq!(run(&args).unwrap(), ExitCode::from(1));
    }

    #[test]
    fn type_pointer_syntax() {
        let (_d, p) = write_tmp(r#"{"a":{"b":true}}"#);
        let args = TypeArgs {
            path: Some("/a/b".to_string()),
            files: vec![p],
            limit: None,
        };
        assert_eq!(run(&args).unwrap(), ExitCode::SUCCESS);
    }

    #[test]
    fn type_each_scalar_kind() {
        for body in [
            r#"null"#,
            r#"true"#,
            r#"42"#,
            r#""hello""#,
            r#"[]"#,
            r#"{}"#,
        ] {
            let (_d, p) = write_tmp(body);
            let args = TypeArgs {
                path: None,
                files: vec![p],
                limit: None,
            };
            assert_eq!(run(&args).unwrap(), ExitCode::SUCCESS);
        }
    }
}
