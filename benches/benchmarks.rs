use std::io::Write;

use criterion::{Criterion, criterion_group, criterion_main};
use globset::Glob;
use regex::Regex;
use tempfile::TempDir;

fn create_large_file(dir: &TempDir, name: &str, lines: usize) -> std::path::PathBuf {
    let path = dir.path().join(name);
    let mut f = std::fs::File::create(&path).unwrap();
    for i in 0..lines {
        writeln!(f, "line {}: the quick brown fox jumps over the lazy dog", i).unwrap();
        if i % 100 == 0 {
            writeln!(f, "line {}: ERROR something went wrong here", i).unwrap();
        }
    }
    path
}

fn create_delimited_file(dir: &TempDir, name: &str, lines: usize) -> std::path::PathBuf {
    let path = dir.path().join(name);
    let mut f = std::fs::File::create(&path).unwrap();
    writeln!(f, "name,age,city,status,score").unwrap();
    for i in 0..lines {
        writeln!(
            f,
            "user_{},{},city_{},active,{}",
            i,
            20 + (i % 50),
            i % 200,
            i * 7 % 1000
        )
        .unwrap();
    }
    path
}

fn create_dir_tree(dir: &TempDir, depth: usize, breadth: usize) {
    fn recurse(base: &std::path::Path, depth: usize, breadth: usize) {
        if depth == 0 {
            return;
        }
        for i in 0..breadth {
            let subdir = base.join(format!("dir_{}", i));
            std::fs::create_dir_all(&subdir).unwrap();
            for j in 0..3 {
                let file = subdir.join(format!("file_{}.rs", j));
                std::fs::write(&file, format!("// file {}/{}", i, j)).unwrap();
            }
            let txt = subdir.join(format!("notes_{}.txt", i));
            std::fs::write(&txt, "some notes").unwrap();
            recurse(&subdir, depth - 1, breadth);
        }
    }
    recurse(dir.path(), depth, breadth);
}

fn bench_grep_single_line(c: &mut Criterion) {
    let dir = TempDir::new().unwrap();
    let path = create_large_file(&dir, "data.txt", 10_000);
    let content: Vec<String> = std::fs::read_to_string(&path)
        .unwrap()
        .lines()
        .map(String::from)
        .collect();
    let re = Regex::new("ERROR").unwrap();

    c.bench_function("grep_single_line_10k", |b| {
        b.iter(|| {
            let mut count = 0usize;
            for line in &content {
                if re.is_match(line) {
                    count += 1;
                }
            }
            std::hint::black_box(count)
        });
    });
}

fn bench_grep_multiline(c: &mut Criterion) {
    let dir = TempDir::new().unwrap();
    let path = create_large_file(&dir, "data.txt", 10_000);
    let content = std::fs::read_to_string(&path).unwrap();
    let re = Regex::new(r"(?s)ERROR.*?wrong").unwrap();

    c.bench_function("grep_multiline_10k", |b| {
        b.iter(|| {
            let count = re.find_iter(&content).count();
            std::hint::black_box(count)
        });
    });
}

fn bench_cut_whitespace(c: &mut Criterion) {
    let dir = TempDir::new().unwrap();
    let path = create_large_file(&dir, "data.txt", 100_000);
    let content = std::fs::read_to_string(&path).unwrap();

    c.bench_function("cut_whitespace_100k", |b| {
        b.iter(|| {
            let mut count = 0usize;
            for line in content.lines() {
                let fields: Vec<&str> = line.split_whitespace().collect();
                if let Some(f) = fields.get(1) {
                    std::hint::black_box(f);
                    count += 1;
                }
            }
            std::hint::black_box(count)
        });
    });
}

fn bench_cut_delimiter(c: &mut Criterion) {
    let dir = TempDir::new().unwrap();
    let path = create_delimited_file(&dir, "data.csv", 100_000);
    let content = std::fs::read_to_string(&path).unwrap();

    c.bench_function("cut_comma_delim_100k", |b| {
        b.iter(|| {
            let mut count = 0usize;
            for line in content.lines() {
                let fields: Vec<&str> = line.splitn(4, ',').collect();
                if let Some(f) = fields.get(2) {
                    std::hint::black_box(f);
                    count += 1;
                }
            }
            std::hint::black_box(count)
        });
    });
}

fn bench_glob_matching(c: &mut Criterion) {
    let dir = TempDir::new().unwrap();
    // 4 levels deep, 5 wide = 5^4 = 625 dirs, ~2500 .rs files + 625 .txt files
    create_dir_tree(&dir, 4, 5);

    let glob = Glob::new("**/*.rs").unwrap().compile_matcher();

    c.bench_function("glob_match_rs_files", |b| {
        b.iter(|| {
            let mut count = 0usize;
            for entry in walkdir::WalkDir::new(dir.path()) {
                if let Ok(e) = entry {
                    if e.file_type().is_file() {
                        let rel = e.path().strip_prefix(dir.path()).unwrap();
                        if glob.is_match(rel) {
                            count += 1;
                        }
                    }
                }
            }
            std::hint::black_box(count)
        });
    });
}

criterion_group!(
    benches,
    bench_grep_single_line,
    bench_grep_multiline,
    bench_cut_whitespace,
    bench_cut_delimiter,
    bench_glob_matching
);
criterion_main!(benches);
