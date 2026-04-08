use std::io;
use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::Result;
use clap::{Args, ValueEnum};

use crate::output::BoundedWriter;

#[derive(Clone, ValueEnum)]
pub enum TagSort {
    Name,
    Date,
}

#[derive(Args)]
#[command(
    about = "List tags",
    long_about = "List tags with optional filtering and sorting.\n\n\
        By default tags are sorted alphabetically by name.",
    after_help = "\
Examples:
  sak git tags                      List all tags
  sak git tags --sort date          Sort by date (newest first)
  sak git tags --pattern 'v*'       Tags matching a glob pattern"
)]
pub struct TagsArgs {
    /// Path to the git repository
    #[arg(short = 'C', long)]
    pub repo: Option<PathBuf>,

    /// Sort order
    #[arg(long, default_value = "name")]
    pub sort: TagSort,

    /// Only tags that contain this commit
    #[arg(long)]
    pub contains: Option<String>,

    /// Glob pattern to filter tag names
    #[arg(long)]
    pub pattern: Option<String>,

    /// Maximum output lines
    #[arg(long)]
    pub limit: Option<usize>,
}

pub fn run(args: &TagsArgs) -> Result<ExitCode> {
    let repo = super::open_repo(&args.repo)?;

    let pattern = args.pattern.as_deref();
    let tag_names = repo.tag_names(pattern)?;

    let contains_oid = if let Some(ref spec) = args.contains {
        let obj = repo
            .revparse_single(spec)
            .map_err(|e| anyhow::anyhow!("cannot resolve '{}': {}", spec, e))?;
        Some(obj.peel_to_commit()?.id())
    } else {
        None
    };

    let mut entries: Vec<(String, i64)> = Vec::new();

    for name in tag_names.iter().flatten() {
        let obj = match repo.revparse_single(name) {
            Ok(obj) => obj,
            Err(_) => continue,
        };

        if let Some(target_oid) = contains_oid {
            let tag_oid = match obj.peel_to_commit() {
                Ok(c) => c.id(),
                Err(_) => continue,
            };
            if tag_oid != target_oid
                && !repo
                    .graph_descendant_of(tag_oid, target_oid)
                    .unwrap_or(false)
            {
                continue;
            }
        }

        let time = obj
            .peel_to_commit()
            .map(|c| c.time().seconds())
            .unwrap_or(0);

        entries.push((name.to_string(), time));
    }

    if entries.is_empty() {
        return Ok(ExitCode::from(1));
    }

    match args.sort {
        TagSort::Name => entries.sort_by(|a, b| a.0.cmp(&b.0)),
        TagSort::Date => entries.sort_by(|a, b| b.1.cmp(&a.1)),
    }

    let stdout = io::stdout();
    let handle = stdout.lock();
    let mut writer = BoundedWriter::new(handle, args.limit);

    for (name, _) in &entries {
        if !writer.write_line(name)? {
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

    fn create_commit_and_tag(repo: &git2::Repository, tag_name: &str) {
        let dir = repo.workdir().unwrap();
        let filename = format!("{}.txt", tag_name);
        fs::write(dir.join(&filename), tag_name).unwrap();
        let mut index = repo.index().unwrap();
        index.add_path(std::path::Path::new(&filename)).unwrap();
        index.write().unwrap();
        let tree_oid = index.write_tree().unwrap();
        let tree = repo.find_tree(tree_oid).unwrap();
        let sig = repo.signature().unwrap();
        let parents: Vec<git2::Commit> = if let Ok(head) = repo.head() {
            vec![head.peel_to_commit().unwrap()]
        } else {
            vec![]
        };
        let parent_refs: Vec<&git2::Commit> = parents.iter().collect();
        let oid = repo
            .commit(Some("HEAD"), &sig, &sig, tag_name, &tree, &parent_refs)
            .unwrap();
        let obj = repo.find_object(oid, None).unwrap();
        repo.tag_lightweight(tag_name, &obj, false).unwrap();
    }

    #[test]
    fn test_tags_empty() {
        let (_dir, repo) = crate::git::init_test_repo();
        let args = TagsArgs {
            repo: Some(repo.workdir().unwrap().to_path_buf()),
            sort: TagSort::Name,
            contains: None,
            pattern: None,
            limit: None,
        };
        let result = run(&args).unwrap();
        assert_eq!(result, ExitCode::from(1));
    }

    #[test]
    fn test_tags_with_tag() {
        let (_dir, repo) = crate::git::init_test_repo();
        create_commit_and_tag(&repo, "v1.0");

        let args = TagsArgs {
            repo: Some(repo.workdir().unwrap().to_path_buf()),
            sort: TagSort::Name,
            contains: None,
            pattern: None,
            limit: None,
        };
        let result = run(&args).unwrap();
        assert_eq!(result, ExitCode::SUCCESS);
    }
}
