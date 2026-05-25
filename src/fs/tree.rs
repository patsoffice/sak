use std::io;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, Result};
use clap::Args;

use super::should_skip;
use crate::output::BoundedWriter;

#[derive(Args)]
#[command(
    about = "Display a directory structure as an indented tree",
    long_about = "Display a directory structure as an indented tree.\n\n\
        Recursively walks PATH (default \".\") and prints it as a tree using the \
        usual `├──`/`└──`/`│` connectors, sorted alphabetically at each level, \
        then a `N directories, M files` summary line. The same directory pruning \
        as `glob` applies (.git, target, node_modules, __pycache__, .venv, and \
        dotfiles unless --hidden). Symlinks are shown as leaves and never \
        descended into.\n\n\
        Use --max-depth to limit how many levels below the root are shown, \
        --dirs-only to list directories only, and --limit to cap the number of \
        emitted lines (the summary is omitted when output is truncated).",
    after_help = "\
Examples:
  sak fs tree                              Tree of the current directory
  sak fs tree src                          Tree rooted at src/
  sak fs tree --max-depth 2                Only two levels below the root
  sak fs tree --dirs-only                  Directories only
  sak fs tree --hidden                     Include dotfiles"
)]
pub struct TreeArgs {
    /// Directory to display (defaults to ".")
    #[arg(default_value = ".")]
    pub path: PathBuf,

    /// Maximum number of levels below the root to show
    #[arg(long, value_name = "N")]
    pub max_depth: Option<usize>,

    /// List directories only (omit files)
    #[arg(long)]
    pub dirs_only: bool,

    /// Include hidden files and directories (dotfiles)
    #[arg(short = 'H', long)]
    pub hidden: bool,

    /// Maximum number of lines to output
    #[arg(long)]
    pub limit: Option<usize>,
}

#[derive(Default)]
struct Counts {
    dirs: usize,
    files: usize,
}

/// Recursively collect the tree lines below `dir`. `prefix` is the string
/// printed before each entry's connector (built up from the ancestors), `level`
/// is 1-based depth below the root. Entries are sorted by file name so output
/// is deterministic. Directory read errors are reported to stderr and skipped.
fn walk(
    dir: &Path,
    prefix: &str,
    level: usize,
    args: &TreeArgs,
    lines: &mut Vec<String>,
    counts: &mut Counts,
) {
    if let Some(md) = args.max_depth
        && level > md
    {
        return;
    }

    let mut entries: Vec<(String, std::fs::FileType)> = match std::fs::read_dir(dir) {
        Ok(rd) => rd
            .filter_map(|e| {
                let e = e.ok()?;
                let name = e.file_name().to_string_lossy().into_owned();
                let ft = e.file_type().ok()?;
                Some((name, ft))
            })
            .collect(),
        Err(e) => {
            eprintln!("sak: error: cannot read {}: {e}", dir.display());
            return;
        }
    };

    // Prune like glob/grep: junk dirs and (unless --hidden) dotfiles. A dotfile
    // that isn't a directory is still a hidden *file*, so guard both kinds.
    entries.retain(|(name, ft)| {
        if ft.is_dir() {
            !should_skip(name, args.hidden)
        } else {
            args.hidden || !name.starts_with('.')
        }
    });

    if args.dirs_only {
        entries.retain(|(_, ft)| ft.is_dir());
    }

    entries.sort_by(|(a, _), (b, _)| a.cmp(b));

    let last = entries.len().saturating_sub(1);
    for (i, (name, ft)) in entries.iter().enumerate() {
        let is_last = i == last;
        let connector = if is_last { "└── " } else { "├── " };
        lines.push(format!("{prefix}{connector}{name}"));

        if ft.is_dir() {
            counts.dirs += 1;
            let child_prefix = format!("{prefix}{}", if is_last { "    " } else { "│   " });
            walk(
                &dir.join(name),
                &child_prefix,
                level + 1,
                args,
                lines,
                counts,
            );
        } else {
            counts.files += 1;
        }
    }
}

pub fn run(args: &TreeArgs) -> Result<ExitCode> {
    let base = args
        .path
        .canonicalize()
        .with_context(|| format!("cannot access directory: {}", args.path.display()))?;
    if !base.is_dir() {
        anyhow::bail!("not a directory: {}", args.path.display());
    }

    let mut lines: Vec<String> = Vec::new();
    let mut counts = Counts::default();
    walk(&base, "", 1, args, &mut lines, &mut counts);

    let stdout = io::stdout();
    let mut writer = BoundedWriter::new(stdout.lock(), args.limit);
    // The root prints as the path the user gave (relative, not canonicalized).
    writer.write_line(&args.path.display().to_string())?;
    let mut full = true;
    for line in &lines {
        if !writer.write_line(line)? {
            full = false;
            break;
        }
    }
    // Only emit the summary when the listing wasn't truncated — a partial tree
    // with a "N directories, M files" footer would misreport what was shown.
    if full {
        let d = if counts.dirs == 1 {
            "directory"
        } else {
            "directories"
        };
        let f = if counts.files == 1 { "file" } else { "files" };
        writer.write_decoration(&format!("\n{} {}, {} {}", counts.dirs, d, counts.files, f))?;
    }
    writer.flush()?;
    Ok(ExitCode::SUCCESS)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn fixture() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::create_dir(root.join("src")).unwrap();
        fs::write(root.join("src/main.rs"), "fn main() {}").unwrap();
        fs::create_dir(root.join("src/fs")).unwrap();
        fs::write(root.join("src/fs/tree.rs"), "// tree").unwrap();
        fs::write(root.join("README.md"), "# readme").unwrap();
        fs::write(root.join(".hidden"), "secret").unwrap();
        fs::create_dir(root.join("target")).unwrap();
        fs::write(root.join("target/junk"), "x").unwrap();
        dir
    }

    fn args(path: &Path) -> TreeArgs {
        TreeArgs {
            path: path.to_path_buf(),
            max_depth: None,
            dirs_only: false,
            hidden: false,
            limit: None,
        }
    }

    fn collect(a: &TreeArgs) -> (Vec<String>, Counts) {
        let base = a.path.canonicalize().unwrap();
        let mut lines = Vec::new();
        let mut counts = Counts::default();
        walk(&base, "", 1, a, &mut lines, &mut counts);
        (lines, counts)
    }

    #[test]
    fn lists_sorted_and_prunes_junk_and_dotfiles() {
        let dir = fixture();
        let (lines, counts) = collect(&args(dir.path()));
        // README.md before src (alphabetical); .hidden and target/ pruned.
        let names: Vec<String> = lines
            .iter()
            .map(|l| l.trim_start_matches(['│', ' ', '├', '└', '─']).to_string())
            .collect();
        assert!(names.contains(&"README.md".to_string()));
        assert!(names.contains(&"src".to_string()));
        assert!(names.contains(&"main.rs".to_string()));
        assert!(!names.iter().any(|n| n == ".hidden"));
        assert!(!names.iter().any(|n| n == "target"));
        // src, src/fs are the two directories; main.rs, tree.rs, README.md files.
        assert_eq!(counts.dirs, 2);
        assert_eq!(counts.files, 3);
    }

    #[test]
    fn max_depth_limits_recursion() {
        let dir = fixture();
        let mut a = args(dir.path());
        a.max_depth = Some(1);
        let (lines, counts) = collect(&a);
        // Only direct children: README.md and src/. No main.rs / fs / tree.rs.
        assert!(!lines.iter().any(|l| l.contains("main.rs")));
        assert_eq!(counts.dirs, 1); // just src
        assert_eq!(counts.files, 1); // just README.md
    }

    #[test]
    fn dirs_only_omits_files() {
        let dir = fixture();
        let mut a = args(dir.path());
        a.dirs_only = true;
        let (lines, counts) = collect(&a);
        assert!(!lines.iter().any(|l| l.contains(".rs")));
        assert_eq!(counts.files, 0);
        assert_eq!(counts.dirs, 2);
    }

    #[test]
    fn hidden_includes_dotfiles() {
        let dir = fixture();
        let mut a = args(dir.path());
        a.hidden = true;
        let (lines, _) = collect(&a);
        assert!(lines.iter().any(|l| l.contains(".hidden")));
    }

    #[test]
    fn connectors_mark_last_child() {
        let dir = fixture();
        let (lines, _) = collect(&args(dir.path()));
        // The last top-level entry (src) uses └──; README.md uses ├──.
        assert!(lines.iter().any(|l| l == "├── README.md"));
        assert!(lines.iter().any(|l| l == "└── src"));
    }

    #[test]
    fn run_succeeds() {
        let dir = fixture();
        assert_eq!(run(&args(dir.path())).unwrap(), ExitCode::SUCCESS);
    }
}
