use crate::output::Outcome;
use std::collections::HashMap;
use std::fs::File;
use std::io::{self, BufReader, Read};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use clap::Args;
use sha2::{Digest, Sha256};

use super::size::parse_size;
use super::{is_hidden_file, pruned_walk};
use crate::output::{BoundedWriter, relative_path};

#[derive(Args)]
#[command(
    about = "Find files with identical contents",
    long_about = "Find files with identical contents (duplicate detection).\n\n\
        Walks PATH (default \".\") and groups files by content. Detection is \
        two-pass: files are first grouped by size (a cheap stat), then only \
        same-size groups are hashed (SHA-256, streamed in 64KB chunks) to \
        confirm an exact byte-for-byte match. Files with a unique size are \
        never read.\n\n\
        Output is one group per set of duplicates, groups separated by a blank \
        line; each line is `size<TAB>hash<TAB>path`. Tiny files are skipped by \
        default (--min-size 1K) because empty/near-empty dupes (e.g. empty \
        __init__.py) are usually noise. The same directory pruning as `glob` \
        applies (.git, target, node_modules, ...; dotfiles unless --hidden).\n\n\
        Exit 1 if no duplicate groups were found, 0 otherwise.",
    after_help = "\
Examples:
  sak fs duplicates                       Find dupes under . (>= 1K)
  sak fs duplicates ~/Downloads           Find dupes under a directory
  sak fs duplicates --min-size 1M         Only consider files >= 1 MiB
  sak fs duplicates --min-size 0          Include tiny files too
  sak fs duplicates --hidden              Include dotfiles"
)]
pub struct DuplicatesArgs {
    /// Directory to search (defaults to ".")
    #[arg(default_value = ".")]
    pub path: PathBuf,

    /// Skip files smaller than this size (default 1K — tiny dupes are noise)
    #[arg(long, value_name = "SIZE", default_value = "1K")]
    pub min_size: String,

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

pub fn run(args: &DuplicatesArgs) -> Result<Outcome> {
    let min_size = parse_size(&args.min_size)
        .with_context(|| format!("invalid --min-size: {}", args.min_size))?;

    let base = args
        .path
        .canonicalize()
        .with_context(|| format!("cannot access directory: {}", args.path.display()))?;

    // Pass 1: bucket candidate files by size. Files with a unique size cannot
    // have a duplicate, so they're never hashed.
    let mut by_size: HashMap<u64, Vec<PathBuf>> = HashMap::new();
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
        by_size
            .entry(len)
            .or_default()
            .push(entry.path().to_path_buf());
    }

    // Pass 2: within each same-size bucket, hash to confirm true duplicates.
    // A group is `(size, hash, sorted paths)` with two or more members.
    let mut groups: Vec<(u64, String, Vec<PathBuf>)> = Vec::new();
    for (size, paths) in by_size {
        if paths.len() < 2 {
            continue;
        }
        let mut by_hash: HashMap<String, Vec<PathBuf>> = HashMap::new();
        for path in paths {
            match hash_file(&path) {
                Ok(hex) => by_hash.entry(hex).or_default().push(path),
                Err(e) => eprintln!("sak: error: {}: {e}", path.display()),
            }
        }
        for (hex, mut members) in by_hash {
            if members.len() < 2 {
                continue;
            }
            members.sort();
            groups.push((size, hex, members));
        }
    }

    if groups.is_empty() {
        return Ok(Outcome::NotFound);
    }

    // Deterministic order: largest groups' wasted space first (size desc),
    // then by hash, so output is stable run-to-run.
    groups.sort_by(|(sa, ha, _), (sb, hb, _)| sb.cmp(sa).then_with(|| ha.cmp(hb)));

    let stdout = io::stdout();
    let mut writer = BoundedWriter::new(stdout.lock(), args.limit);
    for (i, (size, hex, members)) in groups.iter().enumerate() {
        if i > 0 {
            writer.write_decoration("")?;
        }
        for path in members {
            let rel = relative_path(path, &base);
            // Raw bytes in the size column — it pairs with an exact hash, so an
            // exact byte count is the honest figure.
            if !writer.write_line(&format!("{size}\t{hex}\t{rel}"))? {
                writer.flush()?;
                return Ok(Outcome::Found);
            }
        }
    }
    writer.flush()?;
    Ok(Outcome::Found)
}

/// Stream a file through SHA-256 in 64KB chunks, returning lowercase hex.
fn hash_file(path: &Path) -> Result<String> {
    let file = File::open(path)?;
    let mut reader = BufReader::new(file);
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 65536];
    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    let digest = hasher.finalize();
    let mut s = String::with_capacity(digest.len() * 2);
    for b in digest {
        s.push_str(&format!("{b:02x}"));
    }
    Ok(s)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_file(dir: &Path, name: &str, contents: &[u8]) {
        let mut f = std::fs::File::create(dir.join(name)).unwrap();
        f.write_all(contents).unwrap();
    }

    fn args(path: &Path) -> DuplicatesArgs {
        DuplicatesArgs {
            path: path.to_path_buf(),
            min_size: "0".to_string(),
            hidden: false,
            follow_links: false,
            limit: None,
        }
    }

    #[test]
    fn finds_identical_contents() {
        let dir = tempfile::tempdir().unwrap();
        write_file(dir.path(), "a.txt", b"hello world contents here");
        write_file(dir.path(), "b.txt", b"hello world contents here");
        write_file(dir.path(), "c.txt", b"different");
        assert_eq!(run(&args(dir.path())).unwrap(), Outcome::Found);
    }

    #[test]
    fn same_size_different_content_is_not_a_dupe() {
        let dir = tempfile::tempdir().unwrap();
        // Same length, different bytes — must not be grouped.
        write_file(dir.path(), "a.txt", b"aaaa");
        write_file(dir.path(), "b.txt", b"bbbb");
        assert_eq!(run(&args(dir.path())).unwrap(), Outcome::NotFound);
    }

    #[test]
    fn no_dupes_is_exit_1() {
        let dir = tempfile::tempdir().unwrap();
        write_file(dir.path(), "a.txt", b"unique one");
        write_file(dir.path(), "b.txt", b"unique two longer");
        assert_eq!(run(&args(dir.path())).unwrap(), Outcome::NotFound);
    }

    #[test]
    fn min_size_skips_tiny() {
        let dir = tempfile::tempdir().unwrap();
        write_file(dir.path(), "a.txt", b"hi");
        write_file(dir.path(), "b.txt", b"hi");
        let mut a = args(dir.path());
        a.min_size = "1K".to_string();
        assert_eq!(run(&a).unwrap(), Outcome::NotFound);
    }

    #[test]
    fn hash_is_stable() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("x");
        write_file(dir.path(), "x", b"abc");
        assert_eq!(
            hash_file(&p).unwrap(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }
}
