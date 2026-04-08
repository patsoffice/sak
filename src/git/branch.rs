use std::io;
use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::Result;
use clap::Args;
use git2::BranchType;

use crate::output::BoundedWriter;

#[derive(Args)]
#[command(
    about = "List branches",
    long_about = "List branches and show the current branch.\n\n\
        The current branch is marked with a * prefix.",
    after_help = "\
Examples:
  sak git branch                    List local branches
  sak git branch --all              List all branches
  sak git branch --remote           List remote branches
  sak git branch --contains HEAD    Branches containing HEAD"
)]
pub struct BranchArgs {
    /// Path to the git repository
    #[arg(short = 'C', long)]
    pub repo: Option<PathBuf>,

    /// List all branches (local and remote)
    #[arg(short = 'a', long)]
    pub all: bool,

    /// List only remote-tracking branches
    #[arg(short = 'r', long)]
    pub remote: bool,

    /// Only branches that contain this commit
    #[arg(long)]
    pub contains: Option<String>,

    /// Maximum output lines
    #[arg(long)]
    pub limit: Option<usize>,
}

pub fn run(args: &BranchArgs) -> Result<ExitCode> {
    let repo = super::open_repo(&args.repo)?;

    let branch_type = if args.all {
        None
    } else if args.remote {
        Some(BranchType::Remote)
    } else {
        Some(BranchType::Local)
    };

    let branches = repo.branches(branch_type)?;

    let contains_oid = if let Some(ref spec) = args.contains {
        let obj = repo
            .revparse_single(spec)
            .map_err(|e| anyhow::anyhow!("cannot resolve '{}': {}", spec, e))?;
        Some(obj.peel_to_commit()?.id())
    } else {
        None
    };

    let mut entries: Vec<(String, bool)> = Vec::new();
    for branch_result in branches {
        let (branch, _btype) = branch_result?;
        let name = branch.name()?.unwrap_or("(invalid utf-8)").to_string();
        let is_head = branch.is_head();

        if let Some(target_oid) = contains_oid {
            let branch_oid = branch.get().peel_to_commit()?.id();
            // Branch contains the commit if commit is an ancestor of branch tip (or equal)
            if branch_oid != target_oid && !repo.graph_descendant_of(branch_oid, target_oid)? {
                continue;
            }
        }

        entries.push((name, is_head));
    }

    if entries.is_empty() {
        return Ok(ExitCode::from(1));
    }

    entries.sort_by(|a, b| a.0.cmp(&b.0));

    let stdout = io::stdout();
    let handle = stdout.lock();
    let mut writer = BoundedWriter::new(handle, args.limit);

    for (name, is_head) in &entries {
        let prefix = if *is_head { "* " } else { "  " };
        if !writer.write_line(&format!("{}{}", prefix, name))? {
            break;
        }
    }
    writer.flush()?;

    Ok(ExitCode::SUCCESS)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn create_initial_commit(repo: &git2::Repository) {
        let mut index = repo.index().unwrap();
        let dir = repo.workdir().unwrap();
        fs::write(dir.join("init.txt"), "init").unwrap();
        index.add_path(std::path::Path::new("init.txt")).unwrap();
        index.write().unwrap();
        let tree_oid = index.write_tree().unwrap();
        let tree = repo.find_tree(tree_oid).unwrap();
        let sig = repo.signature().unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, "initial commit", &tree, &[])
            .unwrap();
    }

    #[test]
    fn test_branch_with_commit() {
        let (_dir, repo) = crate::git::init_test_repo();
        create_initial_commit(&repo);

        let args = BranchArgs {
            repo: Some(repo.workdir().unwrap().to_path_buf()),
            all: false,
            remote: false,
            contains: None,
            limit: None,
        };
        let result = run(&args).unwrap();
        assert_eq!(result, ExitCode::SUCCESS);
    }

    #[test]
    fn test_branch_empty_repo() {
        let (_dir, repo) = crate::git::init_test_repo();

        let args = BranchArgs {
            repo: Some(repo.workdir().unwrap().to_path_buf()),
            all: false,
            remote: false,
            contains: None,
            limit: None,
        };
        let result = run(&args).unwrap();
        assert_eq!(result, ExitCode::from(1));
    }
}
