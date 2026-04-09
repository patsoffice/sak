use std::io::{self, Read};
use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{Context, Result};
use clap::Args;

use crate::config::{Format, detect_format, parse_one};
use crate::output::BoundedWriter;

#[derive(Args)]
#[command(
    about = "Check whether files contain valid TOML, YAML, or plist",
    long_about = "Check whether files contain valid TOML, YAML, or plist and \
        report parse errors.\n\n\
        Format is auto-detected from the file extension or set explicitly with \
        `--format`. Reads from stdin if no files are given (requires `--format`). \
        Exits 0 if all inputs are valid, exits 1 if any input is invalid. \
        Errors are reported to stderr as `filename:line:column: message` (line \
        and column may be omitted when the parser cannot pinpoint a location).",
    after_help = "\
Examples:
  sak config validate Cargo.toml
  sak config validate config.yaml
  sak config validate Info.plist
  sak config validate --quiet config.toml          Exit code only
  echo 'a: 1' | sak config validate --format yaml  Validate piped input"
)]
pub struct ValidateArgs {
    /// Input files (reads stdin if omitted)
    pub files: Vec<PathBuf>,

    /// Exit code only, no output
    #[arg(short, long)]
    pub quiet: bool,

    /// Force a specific format (required for stdin)
    #[arg(short, long, value_enum)]
    pub format: Option<Format>,

    /// Maximum number of output lines
    #[arg(long)]
    pub limit: Option<usize>,
}

pub fn run(args: &ValidateArgs) -> Result<ExitCode> {
    let stdout = io::stdout();
    let handle = stdout.lock();
    let mut writer = BoundedWriter::new(handle, args.limit);

    let mut any_invalid = false;

    if args.files.is_empty() {
        let fmt = args
            .format
            .context("--format is required when reading from stdin")?;
        let mut buf = Vec::new();
        io::stdin()
            .read_to_end(&mut buf)
            .context("error reading stdin")?;
        match parse_one(fmt, &buf) {
            Ok(_) => {
                if !args.quiet {
                    writer.write_line("<stdin>: valid")?;
                }
            }
            Err(e) => {
                any_invalid = true;
                if !args.quiet {
                    eprintln!("<stdin>:{}", e);
                }
            }
        }
    } else {
        for path in &args.files {
            let name = path.display().to_string();
            let fmt = match detect_format(path, args.format) {
                Ok(f) => f,
                Err(e) => {
                    any_invalid = true;
                    if !args.quiet {
                        eprintln!("{}: {}", name, e);
                    }
                    continue;
                }
            };
            let bytes = match std::fs::read(path) {
                Ok(b) => b,
                Err(e) => {
                    any_invalid = true;
                    if !args.quiet {
                        eprintln!("{}: cannot read: {}", name, e);
                    }
                    continue;
                }
            };
            match parse_one(fmt, &bytes) {
                Ok(_) => {
                    if !args.quiet && !writer.write_line(&format!("{}: valid", name))? {
                        break;
                    }
                }
                Err(e) => {
                    any_invalid = true;
                    if !args.quiet {
                        eprintln!("{}:{}", name, e);
                    }
                }
            }
        }
    }

    writer.flush()?;
    if any_invalid {
        Ok(ExitCode::from(1))
    } else {
        Ok(ExitCode::SUCCESS)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_tmp(name: &str, content: &str) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join(name);
        let mut f = std::fs::File::create(&p).unwrap();
        f.write_all(content.as_bytes()).unwrap();
        (dir, p)
    }

    #[test]
    fn valid_toml() {
        let (_d, p) = write_tmp("a.toml", "a = 1\n");
        let args = ValidateArgs {
            files: vec![p],
            quiet: true,
            format: None,
            limit: None,
        };
        assert_eq!(run(&args).unwrap(), ExitCode::SUCCESS);
    }

    #[test]
    fn invalid_toml() {
        let (_d, p) = write_tmp("a.toml", "a = =\n");
        let args = ValidateArgs {
            files: vec![p],
            quiet: true,
            format: None,
            limit: None,
        };
        assert_eq!(run(&args).unwrap(), ExitCode::from(1));
    }

    #[test]
    fn valid_yaml() {
        let (_d, p) = write_tmp("a.yaml", "a: 1\n");
        let args = ValidateArgs {
            files: vec![p],
            quiet: true,
            format: None,
            limit: None,
        };
        assert_eq!(run(&args).unwrap(), ExitCode::SUCCESS);
    }

    #[test]
    fn invalid_yaml() {
        let (_d, p) = write_tmp("a.yaml", "key: [1, 2\n");
        let args = ValidateArgs {
            files: vec![p],
            quiet: true,
            format: None,
            limit: None,
        };
        assert_eq!(run(&args).unwrap(), ExitCode::from(1));
    }
}
