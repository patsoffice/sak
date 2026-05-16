use std::io::{self, BufReader, Read};
use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{Context, Result, anyhow, bail};
use clap::Args;
use regex::Regex;

use crate::output::BoundedWriter;

use super::headers::parse_delimiter;

#[derive(Args)]
#[command(
    about = "Select columns and filter rows from CSV",
    long_about = "Select columns and filter rows from CSV input.\n\n\
        Output is CSV with the same delimiter as the input. With a header \
        row (the default), columns can be referenced by name or by 1-based \
        index; with --no-header, only indices are valid. Multiple --filter \
        and --filter-regex flags compose with AND semantics. Reads stdin \
        when no files are given; with multiple files, the header is written \
        once from the first file and subsequent files' headers are skipped.",
    after_help = "\
Examples:
  sak csv query -c name,age data.csv               Project two named columns
  sak csv query -c 1,3 data.csv                    Project by 1-based index
  sak csv query --filter status=error log.csv      Rows where status equals 'error'
  sak csv query --filter-regex 'host=^web' log.csv Regex filter
  sak csv query --no-header -c 1,2 raw.csv         No header; index-only refs
  sak csv query -d ';' -c name data.csv            Semicolon delimiter
  cat data.csv | sak csv query -c name             Read from stdin"
)]
pub struct QueryArgs {
    /// Input CSV files (reads stdin if omitted)
    pub files: Vec<PathBuf>,

    /// Comma-separated column names or 1-based indices to select (default: all)
    #[arg(short = 'c', long = "columns")]
    pub columns: Option<String>,

    /// Keep rows where <column>=<exact value>. Repeatable; combined with AND.
    #[arg(long = "filter")]
    pub filter: Vec<String>,

    /// Keep rows where <column> matches <regex>. Repeatable; combined with AND.
    #[arg(long = "filter-regex")]
    pub filter_regex: Vec<String>,

    /// Field delimiter (must be a single byte; default: ',')
    #[arg(short = 'd', long = "delimiter", default_value = ",")]
    pub delimiter: String,

    /// Treat the first row as data, not a header (index-only column refs)
    #[arg(long = "no-header")]
    pub no_header: bool,

    /// Maximum number of output lines (excludes the header row)
    #[arg(long)]
    pub limit: Option<usize>,
}

#[derive(Debug, Clone)]
enum ColumnRef {
    Name(String),
    Index(usize), // 0-based
}

fn parse_column_ref(s: &str) -> Result<ColumnRef> {
    let trimmed = s.trim();
    if trimmed.is_empty() {
        bail!("empty column reference");
    }
    // Plain unsigned integer → index. Anything else → name (even if it looks
    // numeric with a sign, since CSV headers are arbitrary text).
    if trimmed.chars().all(|c| c.is_ascii_digit()) {
        let n: usize = trimmed.parse().context("invalid column index")?;
        if n == 0 {
            bail!("column indices are 1-based");
        }
        return Ok(ColumnRef::Index(n - 1));
    }
    Ok(ColumnRef::Name(trimmed.to_string()))
}

fn parse_columns_spec(spec: &str) -> Result<Vec<ColumnRef>> {
    spec.split(',').map(parse_column_ref).collect()
}

#[derive(Debug, Clone)]
enum FilterKind {
    Equals(String),
    Regex(Regex),
}

struct Filter {
    column: ColumnRef,
    kind: FilterKind,
}

fn parse_filter(spec: &str, is_regex: bool) -> Result<Filter> {
    let (col, val) = spec
        .split_once('=')
        .with_context(|| format!("filter must be of the form col=value: {:?}", spec))?;
    let column = parse_column_ref(col)?;
    let kind = if is_regex {
        FilterKind::Regex(
            Regex::new(val).with_context(|| format!("invalid regex in filter: {:?}", val))?,
        )
    } else {
        FilterKind::Equals(val.to_string())
    };
    Ok(Filter { column, kind })
}

fn resolve_column(r: &ColumnRef, headers: Option<&::csv::StringRecord>) -> Result<usize> {
    match r {
        ColumnRef::Index(i) => Ok(*i),
        ColumnRef::Name(n) => {
            let h = headers.ok_or_else(|| {
                anyhow!(
                    "column name {:?} requires a header row — drop --no-header or use an index",
                    n
                )
            })?;
            h.iter()
                .position(|c| c == n)
                .with_context(|| format!("column not found in header: {:?}", n))
        }
    }
}

fn filter_matches(filters: &[(usize, FilterKind)], rec: &::csv::StringRecord) -> bool {
    for (idx, kind) in filters {
        let field = rec.get(*idx).unwrap_or("");
        let ok = match kind {
            FilterKind::Equals(v) => field == v.as_str(),
            FilterKind::Regex(re) => re.is_match(field),
        };
        if !ok {
            return false;
        }
    }
    true
}

/// Encode a single record as a CSV line (no trailing newline), using the
/// `csv` crate's writer so quoting is RFC-4180 compliant.
fn encode_record<I, S>(fields: I, delim: u8) -> String
where
    I: IntoIterator<Item = S>,
    S: AsRef<[u8]>,
{
    let mut buf = Vec::new();
    {
        let mut w = ::csv::WriterBuilder::new()
            .delimiter(delim)
            .terminator(::csv::Terminator::Any(b'\n'))
            .from_writer(&mut buf);
        w.write_record(fields.into_iter().map(|f| f.as_ref().to_owned()))
            .expect("csv write to Vec cannot fail");
        w.flush().expect("csv flush to Vec cannot fail");
    }
    let s = String::from_utf8(buf).expect("csv writer emits utf-8 for utf-8 input");
    s.trim_end_matches('\n').to_string()
}

struct Plan {
    /// Selected output columns (0-based indices into each input record).
    /// `None` means "project every column" (passthrough).
    select: Option<Vec<usize>>,
    /// Pre-resolved filters: column index + matcher.
    filters: Vec<(usize, FilterKind)>,
    /// Header names for the output, in projection order. Empty when
    /// `--no-header` is set so no header line is emitted.
    output_header: Vec<String>,
}

fn build_plan(args: &QueryArgs, headers: Option<&::csv::StringRecord>) -> Result<Plan> {
    let select: Option<Vec<usize>> = match &args.columns {
        Some(spec) => Some(
            parse_columns_spec(spec)?
                .iter()
                .map(|r| resolve_column(r, headers))
                .collect::<Result<Vec<_>>>()?,
        ),
        None => None,
    };

    let mut filters: Vec<(usize, FilterKind)> = Vec::new();
    for f in &args.filter {
        let parsed = parse_filter(f, false)?;
        let idx = resolve_column(&parsed.column, headers)?;
        filters.push((idx, parsed.kind));
    }
    for f in &args.filter_regex {
        let parsed = parse_filter(f, true)?;
        let idx = resolve_column(&parsed.column, headers)?;
        filters.push((idx, parsed.kind));
    }

    let output_header: Vec<String> = match (&select, headers) {
        (Some(idxs), Some(h)) => idxs
            .iter()
            .map(|&i| h.get(i).unwrap_or("").to_string())
            .collect(),
        (None, Some(h)) => h.iter().map(|s| s.to_string()).collect(),
        // --no-header: no output header line.
        (_, None) => Vec::new(),
    };

    Ok(Plan {
        select,
        filters,
        output_header,
    })
}

fn process_source<R: Read>(
    source: &str,
    reader: R,
    delimiter: u8,
    args: &QueryArgs,
    writer: &mut BoundedWriter<'_>,
    header_written: &mut bool,
) -> Result<bool> {
    let mut rdr = ::csv::ReaderBuilder::new()
        .delimiter(delimiter)
        .has_headers(!args.no_header)
        .flexible(true)
        .from_reader(reader);

    let headers_owned = if !args.no_header {
        Some(
            rdr.headers()
                .with_context(|| format!("reading header from {}", source))?
                .clone(),
        )
    } else {
        None
    };
    let plan = build_plan(args, headers_owned.as_ref())?;

    if !*header_written && !plan.output_header.is_empty() {
        let line = encode_record(plan.output_header.iter(), delimiter);
        writer.write_decoration(&line)?;
        *header_written = true;
    }

    for rec in rdr.records() {
        let rec = rec.with_context(|| format!("reading record from {}", source))?;
        if !filter_matches(&plan.filters, &rec) {
            continue;
        }
        let projected: Vec<&str> = match &plan.select {
            Some(idxs) => idxs.iter().map(|&i| rec.get(i).unwrap_or("")).collect(),
            None => rec.iter().collect(),
        };
        let line = encode_record(projected, delimiter);
        if !writer.write_line(&line)? {
            return Ok(false);
        }
    }
    Ok(true)
}

pub fn run(args: &QueryArgs) -> Result<ExitCode> {
    let delim = parse_delimiter(&args.delimiter)?;
    let stdout = io::stdout();
    let handle = stdout.lock();
    let mut writer = BoundedWriter::new(handle, args.limit);
    let mut header_written = false;

    if args.files.is_empty() {
        let stdin = io::stdin();
        let reader = stdin.lock();
        process_source(
            "<stdin>",
            reader,
            delim,
            args,
            &mut writer,
            &mut header_written,
        )?;
    } else {
        for path in &args.files {
            let file = std::fs::File::open(path)
                .with_context(|| format!("cannot open: {}", path.display()))?;
            let cont = process_source(
                &path.display().to_string(),
                BufReader::new(file),
                delim,
                args,
                &mut writer,
                &mut header_written,
            )?;
            if !cont {
                break;
            }
        }
    }

    writer.flush().context("flushing stdout")?;
    Ok(ExitCode::SUCCESS)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args_for(file: PathBuf, columns: Option<&str>) -> QueryArgs {
        QueryArgs {
            files: vec![file],
            columns: columns.map(str::to_string),
            filter: Vec::new(),
            filter_regex: Vec::new(),
            delimiter: ",".to_string(),
            no_header: false,
            limit: None,
        }
    }

    #[test]
    fn parse_column_ref_index() {
        match parse_column_ref("3").unwrap() {
            ColumnRef::Index(i) => assert_eq!(i, 2),
            _ => panic!("expected index"),
        }
    }

    #[test]
    fn parse_column_ref_name() {
        match parse_column_ref("age").unwrap() {
            ColumnRef::Name(n) => assert_eq!(n, "age"),
            _ => panic!("expected name"),
        }
    }

    #[test]
    fn parse_column_ref_zero_rejected() {
        assert!(parse_column_ref("0").is_err());
    }

    #[test]
    fn parse_filter_equals() {
        let f = parse_filter("status=error", false).unwrap();
        match (f.column, f.kind) {
            (ColumnRef::Name(n), FilterKind::Equals(v)) => {
                assert_eq!(n, "status");
                assert_eq!(v, "error");
            }
            _ => panic!("unexpected filter shape"),
        }
    }

    #[test]
    fn parse_filter_regex_compiles() {
        let f = parse_filter("host=^web", true).unwrap();
        match f.kind {
            FilterKind::Regex(re) => {
                assert!(re.is_match("web-1"));
                assert!(!re.is_match("db-1"));
            }
            _ => panic!("expected regex"),
        }
    }

    #[test]
    fn encode_record_quotes_when_needed() {
        // No quoting necessary
        assert_eq!(encode_record(["a", "b"], b','), "a,b");
        // Field containing delimiter gets quoted
        assert_eq!(encode_record(["a,b", "c"], b','), "\"a,b\",c");
        // Field containing double-quote gets quote-escaped
        assert_eq!(encode_record(["a\"b", "c"], b','), "\"a\"\"b\",c");
    }

    #[test]
    fn name_ref_without_header_errors() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.csv");
        std::fs::write(&p, "1,2,3\n4,5,6\n").unwrap();
        let mut args = args_for(p, Some("name"));
        args.no_header = true;
        assert!(run(&args).is_err());
    }

    #[test]
    fn missing_column_errors() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.csv");
        std::fs::write(&p, "a,b\n1,2\n").unwrap();
        let args = args_for(p, Some("zzz"));
        assert!(run(&args).is_err());
    }

    #[test]
    fn projection_and_filter_run() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.csv");
        std::fs::write(&p, "name,role,city\nalice,admin,nyc\nbob,user,sf\n").unwrap();
        let mut args = args_for(p, Some("name,city"));
        args.filter = vec!["role=admin".to_string()];
        assert_eq!(run(&args).unwrap(), ExitCode::SUCCESS);
    }

    #[test]
    fn no_header_projects_by_index() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.csv");
        std::fs::write(&p, "1,2,3\n4,5,6\n").unwrap();
        let mut args = args_for(p, Some("1,3"));
        args.no_header = true;
        assert_eq!(run(&args).unwrap(), ExitCode::SUCCESS);
    }
}
