use crate::output::Outcome;
use std::io;
use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::Args;
use git2::{Diff, DiffFormat, DiffOptions};

use crate::output::BoundedWriter;

#[derive(Args)]
#[command(
    about = "Show file diffs",
    long_about = "Show file diffs between index, working directory, and commits.\n\n\
        Supports staged vs unstaged, commit ranges, and path filtering.\n\n\
        A ref can be given positionally or with --commit, and may be a range: \
        'A..B' diffs A against B, and 'A...B' diffs the merge base of A and B \
        against B — the standard way to size a branch against its fork point. \
        Either side may be omitted to mean HEAD ('main...' is 'main...HEAD'). \
        A bare ref with no range operator diffs that ref against the working \
        directory.",
    after_help = "\
Examples:
  sak git diff                       Unstaged changes
  sak git diff --staged              Staged changes
  sak git diff --name-only           Changed file names only
  sak git diff HEAD~3                Changes since 3 commits ago
  sak git diff HEAD~3 HEAD           Changes between two commits
  sak git diff main...HEAD --stat    Size this branch since its fork point
  sak git diff -- src/               Changes in src/ only"
)]
pub struct DiffArgs {
    /// Path to the git repository
    #[arg(short = 'C', long)]
    pub repo: Option<PathBuf>,

    /// Show staged changes (index vs HEAD)
    #[arg(short = 's', long)]
    pub staged: bool,

    /// Show only file names that changed
    #[arg(long)]
    pub name_only: bool,

    /// Show stat summary instead of full diff
    #[arg(long)]
    pub stat: bool,

    /// First commit ref, or a range ('A..B', 'A...B'); compared against the
    /// working dir when given alone
    #[arg(long)]
    pub commit: Option<String>,

    /// Second commit ref (compare commit..commit2)
    #[arg(long)]
    pub commit2: Option<String>,

    /// Same as --commit / --commit2, given positionally (`sak git diff A B`)
    #[arg(value_name = "REF", conflicts_with_all = ["commit", "commit2"])]
    pub refs: Vec<String>,

    /// Paths to restrict the diff to
    #[arg(last = true)]
    pub paths: Vec<PathBuf>,

    /// Maximum output lines
    #[arg(long)]
    pub limit: Option<usize>,
}

pub fn run(args: &DiffArgs) -> Result<Outcome> {
    let repo = super::open_repo(&args.repo)?;

    let mut opts = DiffOptions::new();
    for path in &args.paths {
        opts.pathspec(path);
    }

    let diff = build_diff(&repo, args, &mut opts)?;

    if diff.deltas().len() == 0 {
        return Ok(Outcome::NotFound);
    }

    let stdout = io::stdout();
    let handle = stdout.lock();
    let mut writer = BoundedWriter::new(handle, args.limit);

    if args.name_only {
        let mut names: Vec<String> = Vec::new();
        for delta in diff.deltas() {
            let path = delta
                .new_file()
                .path()
                .or_else(|| delta.old_file().path())
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_default();
            names.push(path);
        }
        names.sort();
        names.dedup();
        for name in &names {
            if !writer.write_line(name)? {
                break;
            }
        }
    } else if args.stat {
        let stats = diff.stats()?;
        let buf = stats.to_buf(git2::DiffStatsFormat::FULL, 80)?;
        let text = buf.as_str().unwrap_or("");
        for line in text.lines() {
            if !writer.write_line(line)? {
                break;
            }
        }
    } else {
        diff.print(DiffFormat::Patch, |_delta, _hunk, line| {
            let prefix = match line.origin() {
                '+' => "+",
                '-' => "-",
                ' ' => " ",
                _ => "",
            };
            let content = std::str::from_utf8(line.content()).unwrap_or("");
            let text = format!("{}{}", prefix, content.trim_end_matches('\n'));
            // Ignore write errors in callback (BoundedWriter handles truncation)
            let _ = writer.write_line(&text);
            true
        })?;
    }

    writer.flush()?;
    Ok(Outcome::Found)
}

/// What a `--commit` / positional ref argument asked for.
///
/// git's range operators are part of the ref *string*, not separate flags, so
/// they have to be split off before handing anything to `revparse_single` —
/// libgit2 rejects `main...HEAD` as an invalid pattern rather than resolving it.
#[derive(Debug, PartialEq, Eq)]
enum RangeSpec<'a> {
    /// A bare ref: diff it against the working directory.
    Single(&'a str),
    /// `A..B` — diff A against B directly.
    TwoDot(&'a str, &'a str),
    /// `A...B` — diff the merge base of A and B against B.
    ThreeDot(&'a str, &'a str),
}

/// Split a ref argument on git's range operators. An omitted side means HEAD,
/// so `main...` is `main...HEAD` and `...main` is `HEAD...main`, matching git.
///
/// `...` is tested before `..` because the two-dot pattern is a prefix of the
/// three-dot one. Refnames can't legally contain `..`, so there's no ambiguity
/// with a ref that merely has dots in it (`v1.2.3` stays `Single`).
fn parse_range(spec: &str) -> RangeSpec<'_> {
    fn side(s: &str) -> &str {
        if s.is_empty() { "HEAD" } else { s }
    }
    if let Some((a, b)) = spec.split_once("...") {
        RangeSpec::ThreeDot(side(a), side(b))
    } else if let Some((a, b)) = spec.split_once("..") {
        RangeSpec::TwoDot(side(a), side(b))
    } else {
        RangeSpec::Single(spec)
    }
}

/// Resolve a ref to its tree, with the ref name in the error context.
fn resolve_tree<'a>(repo: &'a git2::Repository, spec: &str) -> Result<git2::Tree<'a>> {
    let obj = repo
        .revparse_single(spec)
        .with_context(|| format!("cannot resolve '{}'", spec))?;
    obj.peel_to_tree()
        .with_context(|| format!("'{}' does not name a tree", spec))
}

fn build_diff<'a>(
    repo: &'a git2::Repository,
    args: &DiffArgs,
    opts: &mut DiffOptions,
) -> Result<Diff<'a>> {
    // Positional refs are the natural spelling; --commit/--commit2 are the
    // older one. clap keeps them mutually exclusive, so at most one is set.
    let spec1 = args
        .commit
        .as_deref()
        .or_else(|| args.refs.first().map(|s| s.as_str()));
    let spec2 = args
        .commit2
        .as_deref()
        .or_else(|| args.refs.get(1).map(|s| s.as_str()));

    if let Some(spec1) = spec1 {
        let range = parse_range(spec1);
        if spec2.is_some() && !matches!(range, RangeSpec::Single(_)) {
            anyhow::bail!(
                "'{}' is already a range; drop the second ref (or the range operator)",
                spec1
            );
        }

        return match range {
            RangeSpec::ThreeDot(a, b) => {
                let oid_a = repo
                    .revparse_single(a)
                    .with_context(|| format!("cannot resolve '{a}'"))?
                    .peel_to_commit()?
                    .id();
                let oid_b = repo
                    .revparse_single(b)
                    .with_context(|| format!("cannot resolve '{b}'"))?
                    .peel_to_commit()?
                    .id();
                let base = repo
                    .merge_base(oid_a, oid_b)
                    .with_context(|| format!("no merge base between '{a}' and '{b}'"))?;
                let base_tree = repo.find_commit(base)?.tree()?;
                let tree_b = resolve_tree(repo, b)?;
                Ok(repo.diff_tree_to_tree(Some(&base_tree), Some(&tree_b), Some(opts))?)
            }
            RangeSpec::TwoDot(a, b) => {
                let tree_a = resolve_tree(repo, a)?;
                let tree_b = resolve_tree(repo, b)?;
                Ok(repo.diff_tree_to_tree(Some(&tree_a), Some(&tree_b), Some(opts))?)
            }
            RangeSpec::Single(a) => {
                let tree_a = resolve_tree(repo, a)?;
                match spec2 {
                    Some(b) => {
                        let tree_b = resolve_tree(repo, b)?;
                        Ok(repo.diff_tree_to_tree(Some(&tree_a), Some(&tree_b), Some(opts))?)
                    }
                    None => Ok(repo.diff_tree_to_workdir(Some(&tree_a), Some(opts))?),
                }
            }
        };
    }

    if args.staged {
        let head = repo.head()?.peel_to_tree()?;
        let index = repo.index()?;
        Ok(repo.diff_tree_to_index(Some(&head), Some(&index), Some(opts))?)
    } else {
        Ok(repo.diff_index_to_workdir(None, Some(opts))?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn create_initial_commit(repo: &git2::Repository) {
        let dir = repo.workdir().unwrap();
        fs::write(dir.join("init.txt"), "init").unwrap();
        let mut index = repo.index().unwrap();
        index.add_path(std::path::Path::new("init.txt")).unwrap();
        index.write().unwrap();
        let tree_oid = index.write_tree().unwrap();
        let tree = repo.find_tree(tree_oid).unwrap();
        let sig = repo.signature().unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, "initial commit", &tree, &[])
            .unwrap();
    }

    /// Commit one new file on top of HEAD.
    fn commit_file(repo: &git2::Repository, name: &str, body: &str) {
        let dir = repo.workdir().unwrap();
        fs::write(dir.join(name), body).unwrap();
        let mut index = repo.index().unwrap();
        index.add_path(std::path::Path::new(name)).unwrap();
        index.write().unwrap();
        let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
        let sig = repo.signature().unwrap();
        let parent = repo.head().unwrap().peel_to_commit().unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, name, &tree, &[&parent])
            .unwrap();
    }

    #[test]
    fn parse_range_splits_on_git_range_operators() {
        assert_eq!(parse_range("HEAD"), RangeSpec::Single("HEAD"));
        // Dots inside a refname must not read as a range.
        assert_eq!(parse_range("v1.2.3"), RangeSpec::Single("v1.2.3"));
        assert_eq!(
            parse_range("HEAD~2..HEAD"),
            RangeSpec::TwoDot("HEAD~2", "HEAD")
        );
        // `...` wins over the `..` prefix it contains.
        assert_eq!(
            parse_range("main...HEAD"),
            RangeSpec::ThreeDot("main", "HEAD")
        );
        // An omitted side means HEAD, as in git.
        assert_eq!(parse_range("main..."), RangeSpec::ThreeDot("main", "HEAD"));
        assert_eq!(parse_range("...main"), RangeSpec::ThreeDot("HEAD", "main"));
        assert_eq!(parse_range("main.."), RangeSpec::TwoDot("main", "HEAD"));
    }

    #[test]
    fn three_dot_diffs_from_the_merge_base() {
        // fork: base -> a (on the branch), and base -> b (on the trunk). A
        // two-dot trunk..branch diff sees b reverted; three-dot does not,
        // because it starts from the merge base instead of the trunk tip.
        let (dir, repo) = crate::git::init_test_repo();
        create_initial_commit(&repo);
        let base = repo.head().unwrap().peel_to_commit().unwrap();
        repo.branch("trunk", &base, false).unwrap();

        commit_file(&repo, "branch.txt", "on the branch");
        repo.set_head("refs/heads/trunk").unwrap();
        repo.checkout_head(Some(git2::build::CheckoutBuilder::new().force()))
            .unwrap();
        commit_file(&repo, "trunk.txt", "on the trunk");
        repo.set_head("refs/heads/master")
            .or_else(|_| repo.set_head("refs/heads/main"))
            .unwrap();

        let workdir = repo.workdir().unwrap().to_path_buf();
        let base_args = DiffArgs {
            repo: Some(workdir),
            staged: false,
            name_only: true,
            stat: false,
            commit: None,
            commit2: None,
            refs: vec![],
            paths: vec![],
            limit: None,
        };

        let mut opts = DiffOptions::new();
        let three = DiffArgs {
            refs: vec!["trunk...HEAD".to_string()],
            ..base_args
        };
        let diff = build_diff(&repo, &three, &mut opts).unwrap();
        let names: Vec<String> = diff
            .deltas()
            .filter_map(|d| d.new_file().path().map(|p| p.display().to_string()))
            .collect();
        // Only the branch's own commit — trunk.txt is on the other side of the
        // fork and must not show up as a deletion.
        assert_eq!(names, ["branch.txt"]);
        drop(dir);
    }

    #[test]
    fn range_plus_second_ref_is_rejected() {
        let (_dir, repo) = crate::git::init_test_repo();
        create_initial_commit(&repo);

        let args = DiffArgs {
            repo: Some(repo.workdir().unwrap().to_path_buf()),
            staged: false,
            name_only: false,
            stat: false,
            commit: Some("HEAD...HEAD".to_string()),
            commit2: Some("HEAD".to_string()),
            refs: vec![],
            paths: vec![],
            limit: None,
        };
        let mut opts = DiffOptions::new();
        assert!(build_diff(&repo, &args, &mut opts).is_err());
    }

    #[test]
    fn test_diff_no_changes() {
        let (_dir, repo) = crate::git::init_test_repo();
        create_initial_commit(&repo);

        let args = DiffArgs {
            repo: Some(repo.workdir().unwrap().to_path_buf()),
            staged: false,
            name_only: false,
            stat: false,
            commit: None,
            commit2: None,
            refs: vec![],
            paths: vec![],
            limit: None,
        };
        let result = run(&args).unwrap();
        assert_eq!(result, Outcome::NotFound);
    }

    #[test]
    fn test_diff_unstaged_changes() {
        let (dir, repo) = crate::git::init_test_repo();
        create_initial_commit(&repo);
        fs::write(dir.path().join("init.txt"), "modified").unwrap();

        let args = DiffArgs {
            repo: Some(repo.workdir().unwrap().to_path_buf()),
            staged: false,
            name_only: false,
            stat: false,
            commit: None,
            commit2: None,
            refs: vec![],
            paths: vec![],
            limit: None,
        };
        let result = run(&args).unwrap();
        assert_eq!(result, Outcome::Found);
    }

    #[test]
    fn test_diff_name_only() {
        let (dir, repo) = crate::git::init_test_repo();
        create_initial_commit(&repo);
        fs::write(dir.path().join("init.txt"), "modified").unwrap();

        let args = DiffArgs {
            repo: Some(repo.workdir().unwrap().to_path_buf()),
            staged: false,
            name_only: true,
            stat: false,
            commit: None,
            commit2: None,
            refs: vec![],
            paths: vec![],
            limit: None,
        };
        let result = run(&args).unwrap();
        assert_eq!(result, Outcome::Found);
    }
}
