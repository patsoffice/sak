//! Test-only helpers shared across domains.
//!
//! Mostly the chokepoint grep-test mechanic: every domain that confines
//! a mutation surface to its `client.rs` (k8s, lxc, docker, prom, gh, talos,
//! sqlite) scans its sibling source files for forbidden tokens. The directory
//! walk and comment-skip logic are identical across all seven, so they live
//! here once. The *token list* and the human-readable summary stay explicit and
//! local at each call site so the guarantee each test makes remains legible.
//!
//! Plus the tree-wide form ([`assert_no_forbidden_tokens_outside`]):
//! guarantees that must hold globally — subprocess spawning confined to the
//! shared `local_bin` chokepoint — get one recursive walk instead of one test
//! per domain.

use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};

/// Assert that none of `forbidden` appears in any `*.rs` file under
/// `src/<domain>/` other than `client.rs`.
///
/// Thin wrapper over [`assert_no_forbidden_tokens_except`] with `client.rs` as
/// the sole exempt file — the common case for the mutation-surface chokepoint.
pub fn assert_no_forbidden_tokens(domain: &str, forbidden: &[&str], summary: &str) {
    assert_no_forbidden_tokens_except(domain, forbidden, &["client.rs"], summary);
}

/// Assert that none of `forbidden` appears in any `*.rs` file under
/// `src/<domain>/` other than the files named in `exempt_files`.
///
/// `domain` is the directory name beneath `src/` (e.g. `"k8s"`);
/// `exempt_files` are bare file names (e.g. `"client.rs"`, `"hook.rs"`) skipped
/// in the scan. Lines whose first non-whitespace characters are `//` (line and
/// doc comments) are exempt, so the chokepoint can be referenced in
/// documentation without tripping the test. On any match, panics with a
/// `path:line: forbidden token` listing prefixed by `summary`.
pub fn assert_no_forbidden_tokens_except(
    domain: &str,
    forbidden: &[&str],
    exempt_files: &[&str],
    summary: &str,
) {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("src")
        .join(domain);
    let entries = fs::read_dir(&dir).unwrap_or_else(|e| panic!("read src/{domain}: {e}"));

    let mut violations = Vec::new();
    for entry in entries {
        let path = entry.expect("dir entry").path();
        if path.extension() != Some(OsStr::new("rs")) {
            continue;
        }
        if exempt_files
            .iter()
            .any(|f| path.file_name() == Some(OsStr::new(f)))
        {
            continue;
        }

        let content = fs::read_to_string(&path).expect("read source file");
        for (idx, line) in content.lines().enumerate() {
            // Skip line comments and doc comments — they're allowed to mention
            // forbidden tokens for documentation purposes.
            if line.trim_start().starts_with("//") {
                continue;
            }
            for token in forbidden {
                if line.contains(token) {
                    violations.push(format!(
                        "{}:{}: forbidden token `{}` outside client.rs",
                        path.display(),
                        idx + 1,
                        token
                    ));
                }
            }
        }
    }

    assert!(
        violations.is_empty(),
        "{summary}:\n{}",
        violations.join("\n")
    );
}

/// Assert that none of `forbidden` appears in any `*.rs` file under `src/`
/// other than the listed `except` files (paths relative to `src/`, e.g.
/// `"local_bin.rs"`, `"hook.rs"`).
///
/// The per-domain [`assert_no_forbidden_tokens_except`] scans one domain
/// directory; this walks the whole tree, so a guarantee that must hold
/// globally — subprocess spawning confined to the shared `local_bin`
/// chokepoint — has one test instead of one per domain. Comment lines are
/// exempt like in the per-domain form. Integration tests under `tests/` are
/// out of scope (they legitimately spawn the built binary).
pub fn assert_no_forbidden_tokens_outside(except: &[&str], forbidden: &[&str], summary: &str) {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    collect_rs_files(&root, &mut files);

    let mut violations = Vec::new();
    for path in files {
        let rel = path
            .strip_prefix(&root)
            .unwrap_or(&path)
            .to_string_lossy()
            .replace('\\', "/");
        if except.iter().any(|e| rel == *e) {
            continue;
        }
        let content = fs::read_to_string(&path).expect("read source file");
        for (idx, line) in content.lines().enumerate() {
            if line.trim_start().starts_with("//") {
                continue;
            }
            for token in forbidden {
                if line.contains(token) {
                    violations.push(format!(
                        "{}:{}: forbidden token `{}` outside {}",
                        path.display(),
                        idx + 1,
                        token,
                        except.join(", ")
                    ));
                }
            }
        }
    }

    assert!(
        violations.is_empty(),
        "{summary}:\n{}",
        violations.join("\n")
    );
}

/// Collect every `*.rs` file under `dir`, recursing into subdirectories.
/// `target/` never lives under `src/`, so no exclusion list is needed.
fn collect_rs_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let entries = fs::read_dir(dir).unwrap_or_else(|e| panic!("read {}: {e}", dir.display()));
    for entry in entries {
        let path = entry.expect("dir entry").path();
        if path.is_dir() {
            collect_rs_files(&path, out);
        } else if path.extension() == Some(OsStr::new("rs")) {
            out.push(path);
        }
    }
}
