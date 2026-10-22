pub mod cut;
pub mod duplicates;
pub mod find;
pub mod glob;
pub mod grep;
pub mod head;
pub mod hook;
pub mod largest;
pub mod read;
pub mod size;
pub mod stat;
pub mod tail;
pub mod tree;
pub mod wc;

use crate::output::Outcome;
use std::path::Path;

use anyhow::Result;
use clap::Subcommand;
use walkdir::{DirEntry, WalkDir};

#[derive(Subcommand)]
pub enum FsCommand {
    Glob(glob::GlobArgs),
    Grep(grep::GrepArgs),
    Cut(cut::CutArgs),
    Read(read::ReadArgs),
    Largest(largest::LargestArgs),
    Duplicates(duplicates::DuplicatesArgs),
    Find(find::FindArgs),
    Tree(tree::TreeArgs),
    Stat(stat::StatArgs),
    Head(head::HeadArgs),
    Tail(tail::TailArgs),
    Wc(wc::WcArgs),
}

pub fn run(cmd: &FsCommand) -> Result<Outcome> {
    match cmd {
        FsCommand::Glob(args) => glob::run(args),
        FsCommand::Grep(args) => grep::run(args),
        FsCommand::Cut(args) => cut::run(args),
        FsCommand::Read(args) => read::run(args),
        FsCommand::Largest(args) => largest::run(args),
        FsCommand::Duplicates(args) => duplicates::run(args),
        FsCommand::Find(args) => find::run(args),
        FsCommand::Tree(args) => tree::run(args),
        FsCommand::Stat(args) => stat::run(args),
        FsCommand::Head(args) => head::run(args),
        FsCommand::Tail(args) => tail::run(args),
        FsCommand::Wc(args) => wc::run(args),
    }
}

/// True if a file argument is the conventional "-" stdin sentinel (cat, grep,
/// ripgrep, ...). `read`, `head`, `tail`, `cut`, and `wc` route a `-` path to
/// stdin; `grep` already honors the same sentinel inline.
pub(crate) fn is_stdin(path: &Path) -> bool {
    path.as_os_str() == "-"
}

/// Whether `fd` carries piped input: a FIFO (`cmd | sak …`) or a regular file
/// (`sak … < file`). Everything else — a TTY, `/dev/null`, a socket, or an fd
/// that can't be stat'd — is not. Sockets are deliberately excluded: agent
/// harnesses built on Node spawn commands over socketpairs with nothing
/// written, so an omitted path must not silently read that empty stream and
/// report "no results". A shell pipe inside the command is still a FIFO.
pub(crate) fn fd_is_piped(fd: std::os::fd::BorrowedFd<'_>) -> bool {
    use std::os::unix::fs::FileTypeExt;

    let Ok(owned) = fd.try_clone_to_owned() else {
        return false;
    };
    let Ok(meta) = std::fs::File::from(owned).metadata() else {
        return false;
    };
    let ft = meta.file_type();
    ft.is_fifo() || ft.is_file()
}

/// [`fd_is_piped`] for this process's stdin: whether an omitted file argument
/// may mean "read stdin". An explicit `-` always reads stdin regardless.
pub(crate) fn stdin_is_piped() -> bool {
    use std::os::fd::AsFd;
    fd_is_piped(std::io::stdin().as_fd())
}

/// Gate for commands whose omitted file argument means stdin: error (exit 2)
/// instead of reading a stdin nothing was piped into, which would otherwise
/// print an empty or zero result that looks like a real answer.
pub(crate) fn require_piped_stdin() -> Result<()> {
    if stdin_is_piped() {
        Ok(())
    } else {
        anyhow::bail!(
            "no file given and stdin is not piped; pass a file path, or '-' to read stdin anyway"
        )
    }
}

/// Display label used for input read from stdin (matches grep/ripgrep). Shared
/// by `grep` and `wc` so stdin is named identically across `fs` commands.
pub(crate) const STDIN_LABEL: &str = "(standard input)";

/// Directories pruned from every recursive walk unless `--hidden` is set
/// (which only un-prunes the dotfile entries, never the junk dirs).
pub(crate) const SKIP_DIRS: &[&str] = &[".git", "target", "node_modules", "__pycache__", ".venv"];

/// Whether a directory entry named `name` should be pruned. Dotfiles are
/// pruned unless `hidden` is set; the junk dirs in [`SKIP_DIRS`] are always
/// pruned. Shared by `glob` and the disk-triage commands.
pub(crate) fn should_skip(name: &str, hidden: bool) -> bool {
    if !hidden && name.starts_with('.') {
        return true;
    }
    SKIP_DIRS.contains(&name)
}

/// Build a [`WalkDir`] iterator rooted at `base` with the shared directory
/// pruning applied via `filter_entry`. The root (`depth() == 0`) is never
/// pruned; below it, directories matching [`should_skip`] are pruned so the
/// walk never descends into them. Hidden *files* are not filtered here — each
/// command applies that to its own entry type via [`is_hidden_file`].
pub(crate) fn pruned_walk(
    base: &Path,
    hidden: bool,
    follow_links: bool,
    max_depth: Option<usize>,
) -> impl Iterator<Item = walkdir::Result<DirEntry>> {
    let mut walker = WalkDir::new(base).follow_links(follow_links);
    if let Some(depth) = max_depth {
        walker = walker.max_depth(depth);
    }
    walker.into_iter().filter_entry(move |e| {
        if e.depth() > 0
            && e.file_type().is_dir()
            && let Some(name) = e.file_name().to_str()
        {
            return !should_skip(name, hidden);
        }
        true
    })
}

/// Whether `entry` is a hidden (dotfile) non-directory that should be skipped
/// when `hidden` is false. Directories are handled by [`pruned_walk`]'s
/// `filter_entry`, so this only guards files and symlinks.
pub(crate) fn is_hidden_file(entry: &DirEntry, hidden: bool) -> bool {
    if hidden || entry.file_type().is_dir() {
        return false;
    }
    entry
        .file_name()
        .to_str()
        .is_some_and(|n| n.starts_with('.'))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::fd::AsFd;

    #[test]
    fn pipes_and_files_are_piped_input() {
        // `cmd | sak …` hands sak a FIFO; `sak … < file` a regular file.
        let (reader, _writer) = std::io::pipe().unwrap();
        assert!(fd_is_piped(reader.as_fd()));
        let file = tempfile::tempfile().unwrap();
        assert!(fd_is_piped(file.as_fd()));
    }

    #[test]
    fn harness_shaped_stdin_is_not_piped_input() {
        // Regression (sak-llm-poqf): Node-based agent harnesses spawn
        // commands over a socketpair, and `< /dev/null` is a char device
        // like a TTY. Neither may stand in for an omitted file argument.
        let (sock, _peer) = std::os::unix::net::UnixStream::pair().unwrap();
        assert!(!fd_is_piped(sock.as_fd()));
        let null = std::fs::File::open("/dev/null").unwrap();
        assert!(!fd_is_piped(null.as_fd()));
    }
}
