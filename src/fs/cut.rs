use std::io::{self, BufRead, BufReader};
use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{Context, Result, bail};
use clap::Args;
use regex::Regex;

use crate::output::BoundedWriter;

#[derive(Args)]
#[command(
    about = "Extract fields from delimited text",
    long_about = "Extract fields from delimited text.\n\n\
        Splits each line by a delimiter and outputs selected fields. \
        Reads from stdin if no files are given. Default delimiter is whitespace.",
    after_help = "\
Examples:
  echo 'alice 30 nyc' | sak fs cut -f 1,3           Extract fields 1 and 3
  sak fs cut -d: -f 1 /etc/passwd                   Extract usernames
  sak fs cut -d ',' -f 2-4 data.csv                 Extract field range
  echo 'a:b:c:d:e' | sak fs cut -d: --max-fields 3  Split into 3: 'a', 'b', 'c:d:e'
  sak fs cut -f 2 --filter '1=error' log.txt        Field 2 where field 1 is 'error'
  cat data.tsv | sak fs cut --header -f name,age    Select by column name"
)]
pub struct CutArgs {
    /// Input files (reads stdin if omitted)
    pub files: Vec<PathBuf>,

    /// Field delimiter (default: split on whitespace runs)
    #[arg(short = 'd', long = "delimiter")]
    pub delimiter: Option<String>,

    /// Use a regex as the delimiter
    #[arg(long = "regex-delim", conflicts_with = "delimiter")]
    pub regex_delim: Option<String>,

    /// Field indices to extract (1-based, e.g., "1,3", "2-5", "3-")
    #[arg(short = 'f', long = "fields", required = true)]
    pub fields: String,

    /// Output field separator
    #[arg(short = 's', long = "separator", default_value = "\t")]
    pub separator: String,

    /// Split into at most N fields; remainder stays unsplit in the last field
    #[arg(long = "max-fields")]
    pub max_fields: Option<usize>,

    /// Treat first line as header; select fields by name
    #[arg(long)]
    pub header: bool,

    /// Skip empty lines
    #[arg(long = "skip-empty")]
    pub skip_empty: bool,

    /// Trim whitespace from fields
    #[arg(long)]
    pub trim: bool,

    /// Deduplicate output lines
    #[arg(long)]
    pub unique: bool,

    /// Keep lines where field matches (e.g., "2=error", "1~^foo")
    #[arg(long)]
    pub filter: Option<String>,

    /// Maximum number of output lines
    #[arg(long)]
    pub limit: Option<usize>,
}

/// Represents which fields to select.
#[derive(Debug)]
enum FieldSpec {
    /// Specific indices (0-based internally)
    Indices(Vec<usize>),
    /// Range from start to end inclusive (0-based)
    Range(usize, Option<usize>),
    /// Named fields (resolved from header)
    Names(Vec<String>),
}

fn parse_field_spec(spec: &str) -> Result<FieldSpec> {
    // Check if it looks like named fields (contains non-numeric, non-separator chars)
    if spec.chars().any(|c| c.is_alphabetic() || c == '_') {
        let names: Vec<String> = spec.split(',').map(|s| s.trim().to_string()).collect();
        return Ok(FieldSpec::Names(names));
    }

    let parts: Vec<&str> = spec.split(',').collect();

    // Single range like "2-5" or "3-"
    if parts.len() == 1 && parts[0].contains('-') {
        let range = parts[0];
        if let Some(stripped) = range.strip_suffix('-') {
            let start: usize = stripped.parse::<usize>().context("invalid field number")?;
            if start == 0 {
                bail!("field numbers are 1-based");
            }
            return Ok(FieldSpec::Range(start - 1, None));
        }
        if let Some((a, b)) = range.split_once('-') {
            let start: usize = a.parse::<usize>().context("invalid field number")?;
            let end: usize = b.parse::<usize>().context("invalid field number")?;
            if start == 0 || end == 0 {
                bail!("field numbers are 1-based");
            }
            return Ok(FieldSpec::Range(start - 1, Some(end - 1)));
        }
    }

    // List of indices: "1,3,5"
    let mut indices = Vec::new();
    for part in parts {
        if part.contains('-') {
            // Expand ranges within comma-separated list
            if let Some((a, b)) = part.split_once('-') {
                let start: usize = a.parse::<usize>().context("invalid field number")?;
                let end: usize = b.parse::<usize>().context("invalid field number")?;
                if start == 0 || end == 0 {
                    bail!("field numbers are 1-based");
                }
                for i in start..=end {
                    indices.push(i - 1);
                }
            }
        } else {
            let idx: usize = part.parse::<usize>().context("invalid field number")?;
            if idx == 0 {
                bail!("field numbers are 1-based");
            }
            indices.push(idx - 1);
        }
    }

    Ok(FieldSpec::Indices(indices))
}

/// Parse a filter expression like "2=error" or "1~^foo"
struct Filter {
    field_idx: usize, // 0-based
    kind: FilterKind,
}

enum FilterKind {
    Equals(String),
    Regex(Regex),
}

fn parse_filter(spec: &str) -> Result<Filter> {
    // Try regex match first: "1~^foo"
    if let Some((field, pattern)) = spec.split_once('~') {
        let idx: usize = field
            .parse::<usize>()
            .context("invalid field number in filter")?;
        if idx == 0 {
            bail!("field numbers are 1-based");
        }
        let re =
            Regex::new(pattern).with_context(|| format!("invalid regex in filter: {}", pattern))?;
        return Ok(Filter {
            field_idx: idx - 1,
            kind: FilterKind::Regex(re),
        });
    }

    // Equals match: "2=error"
    if let Some((field, value)) = spec.split_once('=') {
        let idx: usize = field
            .parse::<usize>()
            .context("invalid field number in filter")?;
        if idx == 0 {
            bail!("field numbers are 1-based");
        }
        return Ok(Filter {
            field_idx: idx - 1,
            kind: FilterKind::Equals(value.to_string()),
        });
    }

    bail!(
        "invalid filter format: expected 'N=value' or 'N~regex', got '{}'",
        spec
    );
}

impl Filter {
    fn matches(&self, fields: &[&str]) -> bool {
        let field = match fields.get(self.field_idx) {
            Some(f) => f,
            None => return false,
        };
        match &self.kind {
            FilterKind::Equals(val) => *field == val.as_str(),
            FilterKind::Regex(re) => re.is_match(field),
        }
    }
}

enum Splitter {
    Whitespace,
    Literal(String),
    Regex(Regex),
}

impl Splitter {
    fn split<'a>(&self, line: &'a str, max_fields: Option<usize>) -> Vec<&'a str> {
        match (self, max_fields) {
            (Splitter::Whitespace, None) => line.split_whitespace().collect(),
            (Splitter::Whitespace, Some(n)) => {
                let mut fields = Vec::with_capacity(n);
                let mut rest = line.trim_start();
                for _ in 0..n - 1 {
                    if rest.is_empty() {
                        break;
                    }
                    match rest.find(|c: char| c.is_whitespace()) {
                        Some(pos) => {
                            fields.push(&rest[..pos]);
                            rest = rest[pos..].trim_start();
                        }
                        None => {
                            fields.push(rest);
                            rest = "";
                            break;
                        }
                    }
                }
                if !rest.is_empty() {
                    fields.push(rest);
                }
                fields
            }
            (Splitter::Literal(delim), None) => line.split(delim.as_str()).collect(),
            (Splitter::Literal(delim), Some(n)) => line.splitn(n, delim.as_str()).collect(),
            (Splitter::Regex(re), None) => re.split(line).collect(),
            (Splitter::Regex(re), Some(n)) => re.splitn(line, n).collect(),
        }
    }
}

fn resolve_field_spec(spec: &FieldSpec, header: Option<&[&str]>) -> Result<Vec<usize>> {
    match spec {
        FieldSpec::Indices(indices) => Ok(indices.clone()),
        FieldSpec::Range(start, end) => {
            // For open ranges, we'll return indices up to the max available fields
            // The caller will handle clamping
            match end {
                Some(e) => Ok((*start..=*e).collect()),
                None => Ok(vec![*start]), // Sentinel: caller handles open range
            }
        }
        FieldSpec::Names(names) => {
            let header = header.context("--header required when using field names")?;
            let mut indices = Vec::new();
            for name in names {
                let idx = header
                    .iter()
                    .position(|h| h == name)
                    .with_context(|| format!("field name '{}' not found in header", name))?;
                indices.push(idx);
            }
            Ok(indices)
        }
    }
}

fn select_fields<'a>(fields: &[&'a str], spec: &FieldSpec, resolved: &[usize]) -> Vec<&'a str> {
    match spec {
        FieldSpec::Range(start, None) => {
            // Open range: from start to end
            fields.iter().skip(*start).copied().collect()
        }
        _ => resolved
            .iter()
            .filter_map(|&i| fields.get(i).copied())
            .collect(),
    }
}

fn process_lines<R: BufRead>(
    reader: R,
    args: &CutArgs,
    splitter: &Splitter,
    spec: &FieldSpec,
    filter: Option<&Filter>,
    writer: &mut BoundedWriter<'_>,
    seen: &mut Option<std::collections::HashSet<String>>,
) -> Result<bool> {
    let mut resolved: Option<Vec<usize>> = None;

    for (i, line) in reader.lines().enumerate() {
        let line = line.context("error reading input")?;

        if args.skip_empty && line.trim().is_empty() {
            continue;
        }

        let fields: Vec<&str> = splitter.split(&line, args.max_fields);

        // Handle header line
        if i == 0 && args.header {
            let trimmed: Vec<String> = fields
                .iter()
                .map(|f| {
                    if args.trim {
                        f.trim().to_string()
                    } else {
                        f.to_string()
                    }
                })
                .collect();
            let refs: Vec<&str> = trimmed.iter().map(|s| s.as_str()).collect();
            resolved = Some(resolve_field_spec(spec, Some(&refs))?);
            continue;
        }

        // Resolve on first data line if no header
        if resolved.is_none() {
            resolved = Some(resolve_field_spec(spec, None)?);
        }

        // Apply filter
        if let Some(f) = filter
            && !f.matches(&fields)
        {
            continue;
        }

        let selected = select_fields(&fields, spec, resolved.as_ref().unwrap());
        let output_fields: Vec<&str> = if args.trim {
            selected.iter().map(|f| f.trim()).collect()
        } else {
            selected
        };

        let output = output_fields.join(&args.separator);

        // Dedup
        if let Some(seen_set) = seen.as_mut()
            && !seen_set.insert(output.clone())
        {
            continue;
        }

        if !writer.write_line(&output)? {
            return Ok(false);
        }
    }

    Ok(true)
}

pub fn run(args: &CutArgs) -> Result<ExitCode> {
    let spec = parse_field_spec(&args.fields)?;

    let splitter = if let Some(ref re_pat) = args.regex_delim {
        let re =
            Regex::new(re_pat).with_context(|| format!("invalid regex delimiter: {}", re_pat))?;
        Splitter::Regex(re)
    } else if let Some(ref delim) = args.delimiter {
        Splitter::Literal(delim.clone())
    } else {
        Splitter::Whitespace
    };

    let filter = args.filter.as_ref().map(|f| parse_filter(f)).transpose()?;

    let stdout = io::stdout();
    let handle = stdout.lock();
    let mut writer = BoundedWriter::new(handle, args.limit);
    let mut seen = if args.unique {
        Some(std::collections::HashSet::new())
    } else {
        None
    };

    if args.files.is_empty() {
        // Read from stdin
        let stdin = io::stdin();
        let reader = stdin.lock();
        process_lines(
            reader,
            args,
            &splitter,
            &spec,
            filter.as_ref(),
            &mut writer,
            &mut seen,
        )?;
    } else {
        for file_path in &args.files {
            let file = std::fs::File::open(file_path)
                .with_context(|| format!("cannot open: {}", file_path.display()))?;
            let reader = BufReader::new(file);
            let cont = process_lines(
                reader,
                args,
                &splitter,
                &spec,
                filter.as_ref(),
                &mut writer,
                &mut seen,
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

    #[test]
    fn test_parse_field_spec_indices() {
        match parse_field_spec("1,3,5").unwrap() {
            FieldSpec::Indices(v) => assert_eq!(v, vec![0, 2, 4]),
            _ => panic!("expected Indices"),
        }
    }

    #[test]
    fn test_parse_field_spec_range() {
        match parse_field_spec("2-5").unwrap() {
            FieldSpec::Range(s, e) => {
                assert_eq!(s, 1);
                assert_eq!(e, Some(4));
            }
            _ => panic!("expected Range"),
        }
    }

    #[test]
    fn test_parse_field_spec_open_range() {
        match parse_field_spec("3-").unwrap() {
            FieldSpec::Range(s, e) => {
                assert_eq!(s, 2);
                assert_eq!(e, None);
            }
            _ => panic!("expected Range"),
        }
    }

    #[test]
    fn test_parse_field_spec_names() {
        match parse_field_spec("name,age").unwrap() {
            FieldSpec::Names(v) => assert_eq!(v, vec!["name", "age"]),
            _ => panic!("expected Names"),
        }
    }

    #[test]
    fn test_parse_field_spec_zero_rejected() {
        assert!(parse_field_spec("0").is_err());
    }

    #[test]
    fn test_splitter_whitespace() {
        let s = Splitter::Whitespace;
        assert_eq!(s.split("a  b  c", None), vec!["a", "b", "c"]);
    }

    #[test]
    fn test_splitter_whitespace_max_fields() {
        let s = Splitter::Whitespace;
        let result = s.split("I hate everything about you", Some(4));
        assert_eq!(result, vec!["I", "hate", "everything", "about you"]);
    }

    #[test]
    fn test_splitter_literal() {
        let s = Splitter::Literal(":".to_string());
        assert_eq!(s.split("a:b:c", None), vec!["a", "b", "c"]);
    }

    #[test]
    fn test_splitter_literal_max_fields() {
        let s = Splitter::Literal(":".to_string());
        assert_eq!(s.split("a:b:c:d:e", Some(3)), vec!["a", "b", "c:d:e"]);
    }

    #[test]
    fn test_splitter_regex() {
        let re = Regex::new(r"[,;]+").unwrap();
        let s = Splitter::Regex(re);
        assert_eq!(s.split("a,,b;c", None), vec!["a", "b", "c"]);
    }

    #[test]
    fn test_filter_equals() {
        let f = parse_filter("2=error").unwrap();
        assert!(f.matches(&["info", "error", "msg"]));
        assert!(!f.matches(&["info", "warn", "msg"]));
    }

    #[test]
    fn test_filter_regex() {
        let f = parse_filter("1~^foo").unwrap();
        assert!(f.matches(&["foobar", "x"]));
        assert!(!f.matches(&["barfoo", "x"]));
    }

    #[test]
    fn test_select_fields() {
        let fields = vec!["a", "b", "c", "d", "e"];
        let spec = FieldSpec::Indices(vec![0, 2, 4]);
        let resolved = vec![0, 2, 4];
        assert_eq!(
            select_fields(&fields, &spec, &resolved),
            vec!["a", "c", "e"]
        );
    }

    #[test]
    fn test_select_fields_open_range() {
        let fields = vec!["a", "b", "c", "d", "e"];
        let spec = FieldSpec::Range(2, None);
        let resolved = vec![2]; // sentinel
        assert_eq!(
            select_fields(&fields, &spec, &resolved),
            vec!["c", "d", "e"]
        );
    }

    #[test]
    fn test_cut_file() {
        use std::io::Write;
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("test.txt");
        {
            let mut f = std::fs::File::create(&file_path).unwrap();
            writeln!(f, "alice 30 nyc").unwrap();
            writeln!(f, "bob 25 sf").unwrap();
        }

        let args = CutArgs {
            files: vec![file_path],
            delimiter: None,
            regex_delim: None,
            fields: "1,3".to_string(),
            separator: "\t".to_string(),
            max_fields: None,
            header: false,
            skip_empty: false,
            trim: false,
            unique: false,
            filter: None,
            limit: None,
        };
        let exit = run(&args).unwrap();
        assert_eq!(exit, ExitCode::SUCCESS);
    }
}
