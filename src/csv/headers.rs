use std::io::{self, BufReader, Read};
use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{Context, Result, bail};
use clap::Args;

use crate::output::BoundedWriter;

#[derive(Args)]
#[command(
    about = "List CSV column headers",
    long_about = "List CSV column names and their 1-based indices, one per line.\n\n\
        Output is `index<TAB>name` (or `index<TAB>name<TAB>type` with --types). \
        Reads from stdin when no files are given. With multiple files, each \
        file's headers are preceded by a `# <path>` decoration line.",
    after_help = "\
Examples:
  sak csv headers data.csv                        List columns and indices
  sak csv headers --types data.csv                Include inferred column types
  sak csv headers -d ';' data.csv                 Use ';' as the delimiter
  sak csv headers --types --sample 500 data.csv   Infer types from 500 rows
  cat data.csv | sak csv headers                  Read from stdin"
)]
pub struct HeadersArgs {
    /// Input CSV files (reads stdin if omitted)
    pub files: Vec<PathBuf>,

    /// Field delimiter (must be a single byte; default: ',')
    #[arg(short = 'd', long = "delimiter", default_value = ",")]
    pub delimiter: String,

    /// Infer column types from sample rows (string, integer, float, bool, empty)
    #[arg(long)]
    pub types: bool,

    /// Rows to sample when inferring types
    #[arg(long, default_value_t = 100)]
    pub sample: usize,

    /// Maximum number of output lines
    #[arg(long)]
    pub limit: Option<usize>,
}

pub(super) fn parse_delimiter(s: &str) -> Result<u8> {
    let bytes = s.as_bytes();
    if bytes.len() != 1 {
        bail!("delimiter must be exactly one byte, got {:?}", s);
    }
    Ok(bytes[0])
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(super) enum InferredType {
    #[default]
    Empty,
    Bool,
    Integer,
    Float,
    String,
}

impl InferredType {
    pub(super) fn promote(self, other: InferredType) -> InferredType {
        use InferredType::*;
        match (self, other) {
            (Empty, t) | (t, Empty) => t,
            (a, b) if a == b => a,
            (Bool, Integer) | (Integer, Bool) => Integer,
            (Bool, Float) | (Float, Bool) => Float,
            (Integer, Float) | (Float, Integer) => Float,
            _ => String,
        }
    }

    pub(super) fn as_str(self) -> &'static str {
        match self {
            InferredType::Empty => "empty",
            InferredType::Bool => "bool",
            InferredType::Integer => "integer",
            InferredType::Float => "float",
            InferredType::String => "string",
        }
    }

    pub(super) fn is_numeric(self) -> bool {
        matches!(self, InferredType::Integer | InferredType::Float)
    }
}

pub(super) fn infer_cell(s: &str) -> InferredType {
    if s.is_empty() {
        return InferredType::Empty;
    }
    let lower = s.to_ascii_lowercase();
    if lower == "true" || lower == "false" {
        return InferredType::Bool;
    }
    if s.parse::<i64>().is_ok() {
        return InferredType::Integer;
    }
    if s.parse::<f64>().is_ok() {
        return InferredType::Float;
    }
    InferredType::String
}

fn process_reader<R: Read>(
    source: &str,
    reader: R,
    delimiter: u8,
    args: &HeadersArgs,
    writer: &mut BoundedWriter<'_>,
    print_source: bool,
) -> Result<bool> {
    let mut rdr = ::csv::ReaderBuilder::new()
        .delimiter(delimiter)
        .has_headers(true)
        .flexible(true)
        .from_reader(reader);

    let headers = rdr
        .headers()
        .with_context(|| format!("reading headers from {}", source))?
        .clone();

    let types: Vec<InferredType> = if args.types {
        let mut acc: Vec<InferredType> = vec![InferredType::Empty; headers.len()];
        for (i, rec) in rdr.records().enumerate() {
            if i >= args.sample {
                break;
            }
            let rec = rec.with_context(|| format!("reading record from {}", source))?;
            for (j, field) in rec.iter().enumerate() {
                if j < acc.len() {
                    acc[j] = acc[j].promote(infer_cell(field));
                }
            }
        }
        acc
    } else {
        Vec::new()
    };

    if print_source {
        writer.write_decoration(&format!("# {}", source))?;
    }

    for (idx, name) in headers.iter().enumerate() {
        let line = if args.types {
            format!(
                "{}\t{}\t{}",
                idx + 1,
                name,
                types
                    .get(idx)
                    .copied()
                    .unwrap_or(InferredType::Empty)
                    .as_str()
            )
        } else {
            format!("{}\t{}", idx + 1, name)
        };
        if !writer.write_line(&line)? {
            return Ok(false);
        }
    }

    Ok(true)
}

pub fn run(args: &HeadersArgs) -> Result<ExitCode> {
    let delim = parse_delimiter(&args.delimiter)?;
    let stdout = io::stdout();
    let handle = stdout.lock();
    let mut writer = BoundedWriter::new(handle, args.limit);

    if args.files.is_empty() {
        let stdin = io::stdin();
        let reader = stdin.lock();
        process_reader("<stdin>", reader, delim, args, &mut writer, false)?;
    } else {
        let multiple = args.files.len() > 1;
        for path in &args.files {
            let file = std::fs::File::open(path)
                .with_context(|| format!("cannot open: {}", path.display()))?;
            let cont = process_reader(
                &path.display().to_string(),
                BufReader::new(file),
                delim,
                args,
                &mut writer,
                multiple,
            )?;
            if !cont {
                break;
            }
        }
    }

    writer.flush()?;
    Ok(ExitCode::SUCCESS)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn parse_delimiter_single_byte() {
        assert_eq!(parse_delimiter(",").unwrap(), b',');
        assert_eq!(parse_delimiter(";").unwrap(), b';');
        assert_eq!(parse_delimiter("\t").unwrap(), b'\t');
    }

    #[test]
    fn parse_delimiter_rejects_multi_byte() {
        assert!(parse_delimiter(",,").is_err());
        assert!(parse_delimiter("").is_err());
    }

    #[test]
    fn infer_cell_basics() {
        assert_eq!(infer_cell(""), InferredType::Empty);
        assert_eq!(infer_cell("42"), InferredType::Integer);
        assert_eq!(infer_cell("-1.5"), InferredType::Float);
        assert_eq!(infer_cell("true"), InferredType::Bool);
        assert_eq!(infer_cell("False"), InferredType::Bool);
        assert_eq!(infer_cell("hello"), InferredType::String);
    }

    #[test]
    fn promotion_int_then_float_widens() {
        assert_eq!(
            InferredType::Integer.promote(InferredType::Float),
            InferredType::Float
        );
        assert_eq!(
            InferredType::Float.promote(InferredType::Integer),
            InferredType::Float
        );
    }

    #[test]
    fn promotion_mismatch_falls_back_to_string() {
        assert_eq!(
            InferredType::Integer.promote(InferredType::String),
            InferredType::String
        );
        assert_eq!(
            InferredType::Bool.promote(InferredType::String),
            InferredType::String
        );
    }

    #[test]
    fn promotion_empty_is_unit() {
        assert_eq!(
            InferredType::Empty.promote(InferredType::Integer),
            InferredType::Integer
        );
        assert_eq!(
            InferredType::Float.promote(InferredType::Empty),
            InferredType::Float
        );
    }

    #[test]
    fn headers_runs_on_file() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.csv");
        {
            let mut f = std::fs::File::create(&p).unwrap();
            writeln!(f, "name,age,city").unwrap();
            writeln!(f, "alice,30,nyc").unwrap();
        }
        let args = HeadersArgs {
            files: vec![p],
            delimiter: ",".to_string(),
            types: true,
            sample: 100,
            limit: None,
        };
        assert_eq!(run(&args).unwrap(), ExitCode::SUCCESS);
    }
}
