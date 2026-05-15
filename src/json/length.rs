use std::io;
use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{Result, anyhow};
use clap::Args;

use crate::json::read_json_inputs;
use crate::output::BoundedWriter;
use crate::value::{resolve_expression, type_name, value_length};

#[derive(Args)]
#[command(
    about = "Print the length of the value at a path",
    long_about = "Print the length of the value at a path: element count for arrays, \
        key count for objects, Unicode scalar (char) count for strings.\n\n\
        Errors on number, boolean, and null values — these have no meaningful \
        length. Exits 1 if the path does not resolve. With no path, the root \
        value is measured. Essential for 'how many X are there' questions \
        without dumping the whole structure. Reads from stdin if no files are \
        given.",
    after_help = "\
Examples:
  echo '[1,2,3]' | sak json length                 3 (array element count)
  echo '{\"a\":1,\"b\":2}' | sak json length         2 (object key count)
  echo '\"hello\"' | sak json length                5 (string char count)
  sak json length .users data.json                  Count users
  sak json length /users/0/tags data.json           JSON Pointer"
)]
pub struct LengthArgs {
    /// Path within the document (default: root)
    pub path: Option<String>,

    /// Input files (reads stdin if omitted)
    pub files: Vec<PathBuf>,

    /// Maximum number of output lines
    #[arg(long)]
    pub limit: Option<usize>,
}

pub fn run(args: &LengthArgs) -> Result<ExitCode> {
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
        let len = value_length(target).ok_or_else(|| {
            anyhow!(
                "value of type '{}' has no length (expected array, object, or string)",
                type_name(target)
            )
        })?;
        if !writer.write_line(&len.to_string())? {
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
    fn length_of_root_array() {
        let (_d, p) = write_tmp(r#"[1,2,3,4]"#);
        let args = LengthArgs {
            path: None,
            files: vec![p],
            limit: None,
        };
        assert_eq!(run(&args).unwrap(), ExitCode::SUCCESS);
    }

    #[test]
    fn length_of_root_object() {
        let (_d, p) = write_tmp(r#"{"a":1,"b":2}"#);
        let args = LengthArgs {
            path: None,
            files: vec![p],
            limit: None,
        };
        assert_eq!(run(&args).unwrap(), ExitCode::SUCCESS);
    }

    #[test]
    fn length_at_path() {
        let (_d, p) = write_tmp(r#"{"users":[1,2,3]}"#);
        let args = LengthArgs {
            path: Some(".users".to_string()),
            files: vec![p],
            limit: None,
        };
        assert_eq!(run(&args).unwrap(), ExitCode::SUCCESS);
    }

    #[test]
    fn length_missing_returns_1() {
        let (_d, p) = write_tmp(r#"{"a":1}"#);
        let args = LengthArgs {
            path: Some(".missing".to_string()),
            files: vec![p],
            limit: None,
        };
        assert_eq!(run(&args).unwrap(), ExitCode::from(1));
    }

    #[test]
    fn length_of_string_is_chars() {
        let (_d, p) = write_tmp(r#""hello""#);
        let args = LengthArgs {
            path: None,
            files: vec![p],
            limit: None,
        };
        assert_eq!(run(&args).unwrap(), ExitCode::SUCCESS);
    }

    #[test]
    fn length_of_scalar_errors() {
        let (_d, p) = write_tmp(r#"42"#);
        let args = LengthArgs {
            path: None,
            files: vec![p],
            limit: None,
        };
        assert!(run(&args).is_err());
    }

    #[test]
    fn length_of_null_errors() {
        let (_d, p) = write_tmp(r#"null"#);
        let args = LengthArgs {
            path: None,
            files: vec![p],
            limit: None,
        };
        assert!(run(&args).is_err());
    }

    #[test]
    fn length_pointer_syntax() {
        let (_d, p) = write_tmp(r#"{"a":{"b":[1,2,3,4,5]}}"#);
        let args = LengthArgs {
            path: Some("/a/b".to_string()),
            files: vec![p],
            limit: None,
        };
        assert_eq!(run(&args).unwrap(), ExitCode::SUCCESS);
    }
}
