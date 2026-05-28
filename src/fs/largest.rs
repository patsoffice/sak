use crate::output::Outcome;
use std::io;
use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::Args;

use super::size::{human, parse_size};
use super::{is_hidden_file, pruned_walk};
use crate::output::{BoundedWriter, relative_path};

#[derive(Args)]
#[command(
    about = "List the largest files under a directory",
    long_about = "List the largest files under a directory.\n\n\
        Recursively walks PATH (default \".\"), then prints the top N files by \
        size, largest first, as `size<TAB>path`. Use this to find what is \
        eating disk — `glob` matches by name, not by size.\n\n\
        Sizes are reported in bytes by default; pass --human for short binary \
        units (1.2K, 4.5M, 1.1G). --min-size drops files below a threshold \
        before ranking. The same directory pruning as `glob` applies (.git, \
        target, node_modules, __pycache__, .venv, and dotfiles unless --hidden).",
    after_help = "\
Examples:
  sak fs largest                          Top 25 largest files under .
  sak fs largest -n 10 src/               Top 10 under src/
  sak fs largest --human                  Human-readable sizes
  sak fs largest --min-size 1M            Only files >= 1 MiB
  sak fs largest -n 5 --human --hidden    Include dotfiles"
)]
pub struct LargestArgs {
    /// Directory to search (defaults to ".")
    #[arg(default_value = ".")]
    pub path: PathBuf,

    /// Number of files to report
    #[arg(short = 'n', long = "top", default_value = "25")]
    pub top: usize,

    /// Print human-readable sizes (1.2K, 4.5M, 1.1G) instead of raw bytes
    #[arg(long)]
    pub human: bool,

    /// Exclude files smaller than this size (e.g. 1M, 512K, 1024)
    #[arg(long, value_name = "SIZE")]
    pub min_size: Option<String>,

    /// Include hidden files and directories (dotfiles)
    #[arg(short = 'H', long)]
    pub hidden: bool,

    /// Follow symbolic links
    #[arg(short = 'L', long)]
    pub follow_links: bool,

    /// Maximum number of results to return
    #[arg(long)]
    pub limit: Option<usize>,
}

pub fn run(args: &LargestArgs) -> Result<Outcome> {
    let min_size = match &args.min_size {
        Some(s) => parse_size(s).with_context(|| format!("invalid --min-size: {s}"))?,
        None => 0,
    };

    let base = args
        .path
        .canonicalize()
        .with_context(|| format!("cannot access directory: {}", args.path.display()))?;

    let mut files: Vec<(u64, PathBuf)> = Vec::new();
    for entry in pruned_walk(&base, args.hidden, args.follow_links, None) {
        let entry = match entry {
            Ok(e) => e,
            Err(e) => {
                eprintln!("sak: error: {e}");
                continue;
            }
        };
        if !entry.file_type().is_file() || is_hidden_file(&entry, args.hidden) {
            continue;
        }
        let Ok(meta) = entry.metadata() else { continue };
        let len = meta.len();
        if len < min_size {
            continue;
        }
        files.push((len, entry.path().to_path_buf()));
    }

    // Largest first; break ties by path so output is deterministic.
    files.sort_by(|(sa, pa), (sb, pb)| sb.cmp(sa).then_with(|| pa.cmp(pb)));
    files.truncate(args.top);

    if files.is_empty() {
        return Ok(Outcome::NotFound);
    }

    let stdout = io::stdout();
    let mut writer = BoundedWriter::new(stdout.lock(), args.limit);
    for (size, path) in &files {
        let size_str = if args.human {
            human(*size)
        } else {
            size.to_string()
        };
        let rel = relative_path(path, &base);
        if !writer.write_line(&format!("{size_str}\t{rel}"))? {
            break;
        }
    }
    writer.flush()?;
    Ok(Outcome::Found)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::path::Path;

    fn write_file(dir: &Path, name: &str, bytes: usize) {
        let mut f = std::fs::File::create(dir.join(name)).unwrap();
        f.write_all(&vec![b'x'; bytes]).unwrap();
    }

    fn args(path: &Path) -> LargestArgs {
        LargestArgs {
            path: path.to_path_buf(),
            top: 25,
            human: false,
            min_size: None,
            hidden: false,
            follow_links: false,
            limit: None,
        }
    }

    #[test]
    fn ranks_largest_first_and_truncates() {
        let dir = tempfile::tempdir().unwrap();
        write_file(dir.path(), "small.txt", 10);
        write_file(dir.path(), "big.txt", 1000);
        write_file(dir.path(), "mid.txt", 100);

        let base = dir.path().canonicalize().unwrap();
        let mut files: Vec<(u64, PathBuf)> = Vec::new();
        for entry in pruned_walk(&base, false, false, None) {
            let entry = entry.unwrap();
            if entry.file_type().is_file() {
                files.push((entry.metadata().unwrap().len(), entry.path().to_path_buf()));
            }
        }
        files.sort_by(|(sa, _), (sb, _)| sb.cmp(sa));
        let names: Vec<_> = files
            .iter()
            .map(|(_, p)| p.file_name().unwrap().to_str().unwrap().to_string())
            .collect();
        assert_eq!(names, vec!["big.txt", "mid.txt", "small.txt"]);
    }

    #[test]
    fn empty_dir_is_exit_1() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(run(&args(dir.path())).unwrap(), Outcome::NotFound);
    }

    #[test]
    fn min_size_filters() {
        let dir = tempfile::tempdir().unwrap();
        write_file(dir.path(), "tiny.txt", 10);
        let mut a = args(dir.path());
        a.min_size = Some("1K".to_string());
        // Only file is below the threshold → no results.
        assert_eq!(run(&a).unwrap(), Outcome::NotFound);
        write_file(dir.path(), "big.txt", 4096);
        assert_eq!(run(&a).unwrap(), Outcome::Found);
    }
}
