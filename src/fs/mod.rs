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
