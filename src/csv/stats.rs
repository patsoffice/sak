use std::io::{self, BufReader, Read};
use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{Context, Result, bail};
use clap::Args;

use crate::output::BoundedWriter;

use super::headers::{InferredType, infer_cell, parse_delimiter};

#[derive(Args)]
#[command(
    about = "Summary statistics for CSV columns",
    long_about = "Print summary statistics for CSV input.\n\n\
        Emits two summary lines (`rows<TAB>n` and `columns<TAB>n`) followed \
        by a tab-separated per-column block:\n\n  \
        index<TAB>name<TAB>type<TAB>nonempty<TAB>empty<TAB>min<TAB>max<TAB>mean\n\n\
        Type is inferred from all sampled cells in the column (string, \
        integer, float, bool, empty). Min / max / mean are populated only \
        for numeric columns (integer or float); non-numeric columns get \
        `-` placeholders. With --sample, only the first N data rows are \
        scanned — useful for large files. Reads stdin when no files are \
        given.",
    after_help = "\
Examples:
  sak csv stats data.csv                    Full-table stats
  sak csv stats --sample 1000 huge.csv      Stats from first 1000 rows
  sak csv stats -c age,salary data.csv      Per-column block limited to two cols
  sak csv stats -d ';' data.csv             Semicolon delimiter
  cat data.csv | sak csv stats              Read from stdin"
)]
pub struct StatsArgs {
    /// Input CSV files (reads stdin if omitted; multiple files not supported)
    pub files: Vec<PathBuf>,

    /// Comma-separated column names or 1-based indices to include in the
    /// per-column block (default: all columns)
    #[arg(short = 'c', long = "columns")]
    pub columns: Option<String>,

    /// Field delimiter (must be a single byte; default: ',')
    #[arg(short = 'd', long = "delimiter", default_value = ",")]
    pub delimiter: String,

    /// Treat the first row as data, not a header (synthesizes col1, col2...)
    #[arg(long = "no-header")]
    pub no_header: bool,

    /// Sample only the first N data rows (default: all)
    #[arg(long)]
    pub sample: Option<usize>,

    /// Maximum number of output lines
    #[arg(long)]
    pub limit: Option<usize>,
}

#[derive(Debug, Clone, Default)]
struct ColumnStats {
    nonempty: u64,
    empty: u64,
    inferred: InferredType,
    /// Running min/max/sum for numeric cells; `count_numeric` is the divisor
    /// for mean. We track these unconditionally and only emit them when the
    /// final inferred type is numeric — keeps the loop branch-free.
    min: f64,
    max: f64,
    sum: f64,
    count_numeric: u64,
}

impl ColumnStats {
    fn new() -> Self {
        Self {
            min: f64::INFINITY,
            max: f64::NEG_INFINITY,
            ..Self::default()
        }
    }

    fn observe(&mut self, cell: &str) {
        let t = infer_cell(cell);
        self.inferred = self.inferred.promote(t);
        match t {
            InferredType::Empty => self.empty += 1,
            _ => self.nonempty += 1,
        }
        if t.is_numeric()
            && let Ok(v) = cell.parse::<f64>()
        {
            self.min = self.min.min(v);
            self.max = self.max.max(v);
            self.sum += v;
            self.count_numeric += 1;
        }
    }

    fn min_str(&self) -> String {
        if self.inferred.is_numeric() && self.count_numeric > 0 {
            format_num(self.min, self.inferred)
        } else {
            "-".to_string()
        }
    }

    fn max_str(&self) -> String {
        if self.inferred.is_numeric() && self.count_numeric > 0 {
            format_num(self.max, self.inferred)
        } else {
            "-".to_string()
        }
    }

    fn mean_str(&self) -> String {
        if self.inferred.is_numeric() && self.count_numeric > 0 {
            let mean = self.sum / (self.count_numeric as f64);
            // Mean is always rendered as float — even on an Integer column,
            // mean of [1, 2] is 1.5, not 1.
            format!("{}", mean)
        } else {
            "-".to_string()
        }
    }
}

/// Render a numeric value with the column's display type — integers stay
/// integer-shaped (no `.0`), floats get the default f64 formatting.
fn format_num(v: f64, t: InferredType) -> String {
    if t == InferredType::Integer && v.fract() == 0.0 && v.is_finite() {
        format!("{}", v as i64)
    } else {
        format!("{}", v)
    }
}

fn resolve_selection(spec: &str, headers: &[String]) -> Result<Vec<usize>> {
    let mut out = Vec::new();
    for token in spec.split(',') {
        let t = token.trim();
        if t.is_empty() {
            bail!("empty column reference");
        }
        if t.chars().all(|c| c.is_ascii_digit()) {
            let n: usize = t.parse().context("invalid column index")?;
            if n == 0 {
                bail!("column indices are 1-based");
            }
            if n > headers.len() {
                bail!(
                    "column index {} out of range (file has {} columns)",
                    n,
                    headers.len()
                );
            }
            out.push(n - 1);
        } else {
            let idx = headers
                .iter()
                .position(|h| h == t)
                .with_context(|| format!("column not found: {:?}", t))?;
            out.push(idx);
        }
    }
    Ok(out)
}

fn compute_stats<R: Read>(
    source: &str,
    reader: R,
    delimiter: u8,
    no_header: bool,
    sample: Option<usize>,
) -> Result<(Vec<String>, Vec<ColumnStats>, u64)> {
    let mut rdr = ::csv::ReaderBuilder::new()
        .delimiter(delimiter)
        .has_headers(!no_header)
        .flexible(true)
        .from_reader(reader);

    let headers: Vec<String> = if !no_header {
        rdr.headers()
            .with_context(|| format!("reading header from {}", source))?
            .iter()
            .map(|s| s.to_string())
            .collect()
    } else {
        Vec::new()
    };

    let mut stats: Vec<ColumnStats> = if headers.is_empty() {
        Vec::new()
    } else {
        (0..headers.len()).map(|_| ColumnStats::new()).collect()
    };

    let mut row_count: u64 = 0;
    for (i, rec) in rdr.records().enumerate() {
        if let Some(n) = sample
            && i >= n
        {
            break;
        }
        let rec = rec.with_context(|| format!("reading record from {}", source))?;
        // Grow stats for --no-header streams where the column count is only
        // known once we see a record. The flexible reader also allows ragged
        // rows, so a late wide row legitimately grows the table.
        if rec.len() > stats.len() {
            stats.resize_with(rec.len(), ColumnStats::new);
        }
        for (j, field) in rec.iter().enumerate() {
            stats[j].observe(field);
        }
        row_count += 1;
    }

    // Synthesize header names when we never had a header row.
    let headers = if headers.is_empty() {
        (1..=stats.len()).map(|i| format!("col{}", i)).collect()
    } else {
        headers
    };

    Ok((headers, stats, row_count))
}

pub fn run(args: &StatsArgs) -> Result<ExitCode> {
    if args.files.len() > 1 {
        bail!("sak csv stats accepts at most one input file");
    }
    let delim = parse_delimiter(&args.delimiter)?;

    let (headers, stats, rows) = if args.files.is_empty() {
        let stdin = io::stdin();
        compute_stats("<stdin>", stdin.lock(), delim, args.no_header, args.sample)?
    } else {
        let path = &args.files[0];
        let file = std::fs::File::open(path)
            .with_context(|| format!("cannot open: {}", path.display()))?;
        compute_stats(
            &path.display().to_string(),
            BufReader::new(file),
            delim,
            args.no_header,
            args.sample,
        )?
    };

    let selection: Vec<usize> = match &args.columns {
        Some(spec) => resolve_selection(spec, &headers)?,
        None => (0..headers.len()).collect(),
    };

    let stdout = io::stdout();
    let handle = stdout.lock();
    let mut writer = BoundedWriter::new(handle, args.limit);

    writer.write_line(&format!("rows\t{}", rows))?;
    writer.write_line(&format!("columns\t{}", headers.len()))?;
    writer.write_decoration("# index\tname\ttype\tnonempty\tempty\tmin\tmax\tmean")?;
    for &i in &selection {
        let name = headers.get(i).cloned().unwrap_or_default();
        let s = stats.get(i).cloned().unwrap_or_else(ColumnStats::new);
        let line = format!(
            "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
            i + 1,
            name,
            s.inferred.as_str(),
            s.nonempty,
            s.empty,
            s.min_str(),
            s.max_str(),
            s.mean_str(),
        );
        if !writer.write_line(&line)? {
            break;
        }
    }
    writer.flush().context("flushing stdout")?;
    Ok(ExitCode::SUCCESS)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(tmp: &std::path::Path, name: &str, body: &str) -> PathBuf {
        let p = tmp.join(name);
        std::fs::write(&p, body).unwrap();
        p
    }

    #[test]
    fn integer_column_min_max_mean() {
        let dir = tempfile::tempdir().unwrap();
        let p = write(
            dir.path(),
            "a.csv",
            "name,age\nalice,30\nbob,40\ncarol,50\n",
        );
        let f = std::fs::File::open(&p).unwrap();
        let (headers, stats, rows) =
            compute_stats("a.csv", BufReader::new(f), b',', false, None).unwrap();
        assert_eq!(rows, 3);
        assert_eq!(headers, vec!["name", "age"]);
        let age = &stats[1];
        assert_eq!(age.inferred, InferredType::Integer);
        assert_eq!(age.nonempty, 3);
        assert_eq!(age.empty, 0);
        assert_eq!(age.min_str(), "30");
        assert_eq!(age.max_str(), "50");
        assert_eq!(age.mean_str(), "40");
    }

    #[test]
    fn string_column_has_no_numeric_stats() {
        let dir = tempfile::tempdir().unwrap();
        let p = write(dir.path(), "a.csv", "name\nalice\nbob\n");
        let f = std::fs::File::open(&p).unwrap();
        let (_, stats, _) = compute_stats("a.csv", BufReader::new(f), b',', false, None).unwrap();
        assert_eq!(stats[0].inferred, InferredType::String);
        assert_eq!(stats[0].min_str(), "-");
        assert_eq!(stats[0].max_str(), "-");
        assert_eq!(stats[0].mean_str(), "-");
    }

    #[test]
    fn empty_cells_counted_separately() {
        // The csv crate skips physically blank lines, so to exercise the
        // empty-cell counter we use an explicit empty field on a multi-column
        // row instead of a blank line between rows.
        let dir = tempfile::tempdir().unwrap();
        let p = write(dir.path(), "a.csv", "x,y\n1,a\n2,\n3,c\n");
        let f = std::fs::File::open(&p).unwrap();
        let (_, stats, rows) =
            compute_stats("a.csv", BufReader::new(f), b',', false, None).unwrap();
        assert_eq!(rows, 3);
        // x: 3 integers, no empties
        assert_eq!(stats[0].nonempty, 3);
        assert_eq!(stats[0].empty, 0);
        assert_eq!(stats[0].inferred, InferredType::Integer);
        // y: 2 strings, 1 empty
        assert_eq!(stats[1].nonempty, 2);
        assert_eq!(stats[1].empty, 1);
        assert_eq!(stats[1].inferred, InferredType::String);
    }

    #[test]
    fn sample_caps_row_count() {
        let dir = tempfile::tempdir().unwrap();
        let p = write(dir.path(), "a.csv", "n\n1\n2\n3\n4\n5\n");
        let f = std::fs::File::open(&p).unwrap();
        let (_, _, rows) = compute_stats("a.csv", BufReader::new(f), b',', false, Some(2)).unwrap();
        assert_eq!(rows, 2);
    }

    #[test]
    fn float_promotion_yields_float_min_max() {
        let dir = tempfile::tempdir().unwrap();
        let p = write(dir.path(), "a.csv", "v\n1\n2.5\n");
        let f = std::fs::File::open(&p).unwrap();
        let (_, stats, _) = compute_stats("a.csv", BufReader::new(f), b',', false, None).unwrap();
        assert_eq!(stats[0].inferred, InferredType::Float);
        // f64 formatting: 1.0 not "1", 2.5 stays 2.5
        assert_eq!(stats[0].min_str(), "1");
        assert_eq!(stats[0].max_str(), "2.5");
    }

    #[test]
    fn no_header_synthesizes_names() {
        let dir = tempfile::tempdir().unwrap();
        let p = write(dir.path(), "a.csv", "1,2\n3,4\n");
        let f = std::fs::File::open(&p).unwrap();
        let (headers, _, rows) =
            compute_stats("a.csv", BufReader::new(f), b',', true, None).unwrap();
        assert_eq!(rows, 2);
        assert_eq!(headers, vec!["col1", "col2"]);
    }

    #[test]
    fn resolve_selection_by_name_and_index() {
        let headers = vec!["name".to_string(), "age".to_string(), "city".to_string()];
        assert_eq!(resolve_selection("name,3", &headers).unwrap(), vec![0, 2]);
    }

    #[test]
    fn resolve_selection_rejects_unknown_name() {
        let headers = vec!["name".to_string()];
        assert!(resolve_selection("zzz", &headers).is_err());
    }

    #[test]
    fn resolve_selection_rejects_oob_index() {
        let headers = vec!["a".to_string(), "b".to_string()];
        assert!(resolve_selection("5", &headers).is_err());
    }

    #[test]
    fn run_emits_summary_lines() {
        let dir = tempfile::tempdir().unwrap();
        let p = write(dir.path(), "a.csv", "x,y\n1,a\n2,b\n");
        let args = StatsArgs {
            files: vec![p],
            columns: None,
            delimiter: ",".to_string(),
            no_header: false,
            sample: None,
            limit: None,
        };
        assert_eq!(run(&args).unwrap(), ExitCode::SUCCESS);
    }

    #[test]
    fn multiple_files_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let p1 = write(dir.path(), "a.csv", "x\n1\n");
        let p2 = write(dir.path(), "b.csv", "x\n1\n");
        let args = StatsArgs {
            files: vec![p1, p2],
            columns: None,
            delimiter: ",".to_string(),
            no_header: false,
            sample: None,
            limit: None,
        };
        assert!(run(&args).is_err());
    }
}
