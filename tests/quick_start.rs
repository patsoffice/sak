//! Runs the offline quick-start examples from `sak --help` against fixtures.
//!
//! Parsing alone can't catch an example with its arguments reversed (both
//! are strings) or a path that doesn't exist in the file it names, so this
//! test executes every example in the domains that need no network, daemon
//! or cluster. It builds a scratch directory holding every file those
//! examples mention (a git repo, a SQLite database, a certificate, ...),
//! runs each example through `sh -c` from there (so quoting and globs work
//! as a reader would type them) and requires exit 0. Examples whose honest
//! answer on the fixtures is "no results" are listed in [`EXPECT_NO_RESULTS`].

use std::path::Path;
use std::process::{Command, Stdio};

/// Domains whose examples run offline against local files.
const OFFLINE_DOMAINS: &[&str] = &[
    "fs", "git", "json", "config", "csv", "cert", "hash", "sqlite",
];

/// Examples that legitimately exit 1 (no results) on the fixtures.
const EXPECT_NO_RESULTS: &[&str] = &[
    // The fixture repo has nothing staged and no commits past `main`.
    "sak git diff --staged",
    "sak git diff main...HEAD --stat",
    // diff(1) semantics: exit 1 means "the documents differ", which the
    // fixture pairs deliberately do.
    "sak json diff a.json b.json",
    "sak config diff a.toml b.yaml",
    // No file in the fixture is a byte-for-byte duplicate.
    "sak fs duplicates",
    // Fixture files were just written, so none is over a megabyte.
    "sak fs find . --size +1M --mtime -7d",
];

/// Examples skipped because they read outside the fixture.
const SKIP: &[&str] = &[
    // Reads the invoking user's real kubeconfig.
    "sak cert from-kubeconfig ~/.kube/config",
];

fn sak() -> &'static str {
    env!("CARGO_BIN_EXE_sak")
}

/// The `  sak …` example commands from `sak --help`, description dropped.
fn quick_start_commands() -> Vec<String> {
    let out = Command::new(sak()).arg("--help").output().unwrap();
    let help = String::from_utf8(out.stdout).unwrap();
    help.lines()
        .filter(|l| l.starts_with("  sak "))
        .map(|l| {
            let body = &l[2..];
            let end = body.find("  ").unwrap_or(body.len());
            body[..end].trim_end().to_string()
        })
        .collect()
}

fn write(dir: &Path, rel: &str, contents: impl AsRef<[u8]>) {
    let path = dir.join(rel);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, contents).unwrap();
}

/// Build the scratch directory every offline example runs in.
fn fixture() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    let repo = Path::new(env!("CARGO_MANIFEST_DIR"));

    // Real manifests, so config examples query real structure.
    std::fs::copy(repo.join("Cargo.toml"), d.join("Cargo.toml")).unwrap();
    std::fs::copy(repo.join("Cargo.lock"), d.join("Cargo.lock")).unwrap();
    write(
        d,
        "src/main.rs",
        "fn main() {\n    let n = Some(1).unwrap();\n    println!(\"{n}\");\n}\n",
    );
    write(d, "src/lib.rs", "pub fn answer() -> u32 {\n    42\n}\n");
    let log: String = (1..=100).map(|i| format!("line {i}\n")).collect();
    write(d, "logfile.txt", log);

    let doc = r#"{"name":"sak","age":3,"issues":[{"id":1,"title":"a"},{"id":2,"title":"b"}]}"#;
    write(d, "data.json", doc);
    write(d, "in.json", doc);
    write(d, "a.json", r#"{"a":1,"b":2}"#);
    write(d, "b.json", r#"{"a":1,"b":3}"#);

    write(
        d,
        "config.yaml",
        "server:\n  port: 8080\n  host: localhost\n",
    );
    write(d, "config.toml", "[server]\nport = 8080\n");
    write(d, "app.ini", "[server]\nport = 8080\n");
    write(d, "a.toml", "[server]\nport = 8080\n");
    write(d, "b.yaml", "server:\n  port: 9090\n");
    write(
        d,
        "Info.plist",
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict><key>CFBundleName</key><string>Sak</string></dict></plist>
"#,
    );
    write(d, "data.csv", "name,age\nalice,30\nbob,25\n");

    std::fs::copy(
        repo.join("src/cert/testdata/sak-test.pem"),
        d.join("cert.pem"),
    )
    .unwrap();
    std::fs::copy(
        repo.join("src/cert/testdata/chain.pem"),
        d.join("chain.pem"),
    )
    .unwrap();
    write(d, "file.tar.gz", b"not really a tarball\n");
    write(d, "disk.iso", b"not really an iso\n");
    let sums = Command::new(sak())
        .args(["hash", "sha256", "file.tar.gz"])
        .current_dir(d)
        .output()
        .unwrap();
    assert!(sums.status.success(), "hashing the sumfile fixture failed");
    write(d, "SHA256SUMS", sums.stdout);
    write(d, "secret.b64", "aGVsbG8gd29ybGQ=\n");
    write(d, "data.bin", b"\x00\x01\x02binary\n");

    #[cfg(feature = "sqlite")]
    {
        let conn = rusqlite::Connection::open(d.join("app.db")).unwrap();
        conn.execute_batch(
            "CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT);
             INSERT INTO users (name) VALUES ('alice'), ('bob');",
        )
        .unwrap();
    }

    // A repo whose only branch is `main`, with every fixture committed over
    // two commits, so `HEAD~1` examples have a parent to compare against.
    let git =
        git2::Repository::init_opts(d, git2::RepositoryInitOptions::new().initial_head("main"))
            .unwrap();
    let who = git2::Signature::now("sak test", "test@example.invalid").unwrap();
    let commit_all = |message: &str| {
        let mut index = git.index().unwrap();
        index
            .add_all(["*"], git2::IndexAddOption::DEFAULT, None)
            .unwrap();
        index.write().unwrap();
        let tree = git.find_tree(index.write_tree().unwrap()).unwrap();
        let parent = git.head().ok().and_then(|h| h.peel_to_commit().ok());
        let parents: Vec<&git2::Commit> = parent.iter().collect();
        git.commit(Some("HEAD"), &who, &who, message, &tree, &parents)
            .unwrap();
    };
    commit_all("fixtures");
    write(d, "CHANGELOG.md", "# Changelog\n\n- second commit\n");
    commit_all("add changelog");
    // Something for `git status` to report.
    write(d, "notes.txt", "untracked\n");

    dir
}

#[test]
fn offline_quick_start_examples_run() {
    let fixture = fixture();
    let commands = quick_start_commands();
    let offline: Vec<&String> = commands
        .iter()
        .filter(|c| {
            let domain = c.split_whitespace().nth(1).unwrap_or("");
            OFFLINE_DOMAINS.contains(&domain) && !SKIP.contains(&c.as_str())
        })
        .collect();
    assert!(!offline.is_empty(), "no offline examples found in --help");

    let mut failures = Vec::new();
    for cmd in offline {
        let script = format!("\"$SAK\" {}", &cmd["sak ".len()..]);
        let out = Command::new("sh")
            .arg("-c")
            .arg(&script)
            .env("SAK", sak())
            .current_dir(fixture.path())
            .stdin(Stdio::null())
            .output()
            .unwrap();
        let code = out.status.code();
        let want = if EXPECT_NO_RESULTS.contains(&cmd.as_str()) {
            1
        } else {
            0
        };
        if code != Some(want) {
            let err = String::from_utf8_lossy(&out.stderr);
            failures.push(format!(
                "{cmd}\n    exit {code:?}, want {want}: {}",
                err.lines().next().unwrap_or("")
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "quick-start examples that fail on the fixtures:\n  {}",
        failures.join("\n  ")
    );
}
