//! `hash` domain — compute cryptographic digests of files or stdin.
//!
//! Hashing isn't filesystem-specific (it also wraps stdin pipes and the
//! base64/DER outputs of `cert`/`sqlite`), so it lives as its own top-level
//! domain rather than under `fs`. The four subcommands (`sha256`, `sha1`,
//! `md5`, `blake3`) share one implementation parameterized by [`Algo`] — they
//! differ only in which hasher they drive.
//!
//! Output matches `shasum`/`sha256sum`: `<hex>  <path>` (two-space separator)
//! so existing tooling can parse it. When reading stdin (no file args) the
//! path column is omitted and just the bare hex digest is printed.
//!
//! All commands are pure computation — no network, no mutation surface — so
//! there is no chokepoint test or read-only enforcement here, mirroring the
//! `cert` domain.

use std::fs::File;
use std::io::{self, BufRead, BufReader, Read};
use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{Context, Result, bail};
use clap::{Args, Subcommand};
use sha2::Digest;

use crate::output::BoundedWriter;

#[derive(Subcommand)]
pub enum HashCommand {
    /// SHA-256 digest (the default choice for new use)
    Sha256(HashArgs),
    /// SHA-1 digest (legacy; collision-broken, kept for compatibility)
    Sha1(HashArgs),
    /// MD5 digest (legacy; collision-broken, kept for compatibility)
    Md5(HashArgs),
    /// BLAKE3 digest (fast modern hash)
    Blake3(HashArgs),
}

/// Which digest a subcommand drives. The three RustCrypto algorithms share the
/// `digest::Digest` trait so [`stream_digest`] handles them generically;
/// BLAKE3 has its own hasher and is dispatched separately.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Algo {
    Sha256,
    Sha1,
    Md5,
    Blake3,
}

#[derive(Args)]
#[command(
    about = "Compute a cryptographic digest of files or stdin",
    long_about = "Compute a cryptographic digest of one or more files (or stdin).\n\n\
        Output matches `shasum`/`sha256sum`: `<hex>  <path>` with a two-space \
        separator, so the output is parseable by existing checksum tooling. \
        Reads stdin when no files are given; in that case just the bare hex \
        digest is printed (no path). Files are streamed in 64KB chunks, so \
        hashing a multi-gigabyte file never buffers it in memory.\n\n\
        Use --binary to suppress the path column and print only the hex \
        digest for every input. Use --verify <sumfile> to check files against \
        a `<hex>  <path>` checksum list (exit 0 if all match, 1 if any \
        mismatch, 2 on a sumfile read/parse error).",
    after_help = "\
Examples:
  sak hash sha256 file.tar.gz                 Hash one file (hex + path)
  sak hash sha256 *.iso                        Hash several files
  cat data.bin | sak hash sha256               Hash stdin (bare hex)
  sak hash blake3 --binary build.zip           Print only the digest
  sak hash sha256 --verify SHA256SUMS          Verify files against a sumfile
  sak k8s get secret tls -o json \\
    | sak json query '.data.\"tls.crt\"' \\
    | sak cert from-yaml - | sak hash sha256    Compose with other domains"
)]
pub struct HashArgs {
    /// Input files (reads stdin if omitted)
    pub files: Vec<PathBuf>,

    /// Print only the hex digest, omitting the two-space separator and path
    #[arg(long)]
    pub binary: bool,

    /// Verify files against a `<hex>  <path>` sumfile instead of hashing
    #[arg(long, value_name = "SUMFILE", conflicts_with = "binary")]
    pub verify: Option<PathBuf>,

    /// Maximum number of output lines
    #[arg(long)]
    pub limit: Option<usize>,
}

pub fn run(cmd: &HashCommand) -> Result<ExitCode> {
    let (algo, args) = match cmd {
        HashCommand::Sha256(a) => (Algo::Sha256, a),
        HashCommand::Sha1(a) => (Algo::Sha1, a),
        HashCommand::Md5(a) => (Algo::Md5, a),
        HashCommand::Blake3(a) => (Algo::Blake3, a),
    };

    if let Some(sumfile) = &args.verify {
        return verify(algo, sumfile, args.limit);
    }

    let stdout = io::stdout();
    let mut writer = BoundedWriter::new(stdout.lock(), args.limit);

    if args.files.is_empty() {
        // Stdin → bare hex, never a path (there is none).
        let hex = hash_reader(algo, io::stdin().lock())?;
        writer.write_line(&hex)?;
        writer.flush()?;
        return Ok(ExitCode::SUCCESS);
    }

    for path in &args.files {
        let file = File::open(path).with_context(|| format!("cannot read: {}", path.display()))?;
        let hex = hash_reader(algo, BufReader::new(file))
            .with_context(|| format!("error hashing {}", path.display()))?;
        let line = if args.binary {
            hex
        } else {
            format!("{}  {}", hex, path.display())
        };
        if !writer.write_line(&line)? {
            break;
        }
    }
    writer.flush()?;
    Ok(ExitCode::SUCCESS)
}

/// Check each `<hex>  <path>` entry in `sumfile` against the freshly computed
/// digest. Emits `<path>: OK` / `<path>: FAILED` per entry (matching
/// `sha256sum --check`). Exit 0 if every entry matched, 1 if any entry failed
/// (mismatch or unreadable file), 2 only if the sumfile itself can't be read.
fn verify(algo: Algo, sumfile: &PathBuf, limit: Option<usize>) -> Result<ExitCode> {
    let f = File::open(sumfile)
        .with_context(|| format!("cannot read sumfile: {}", sumfile.display()))?;
    let reader = BufReader::new(f);

    let stdout = io::stdout();
    let mut writer = BoundedWriter::new(stdout.lock(), limit);

    let mut any_failed = false;
    let mut any_checked = false;
    for (lineno, line) in reader.lines().enumerate() {
        let line = line.with_context(|| format!("error reading sumfile line {}", lineno + 1))?;
        let Some((expected, path)) = parse_sum_line(&line) else {
            continue;
        };
        any_checked = true;

        let status = match File::open(path) {
            Ok(file) => match hash_reader(algo, BufReader::new(file)) {
                Ok(actual) if actual.eq_ignore_ascii_case(expected) => "OK",
                Ok(_) => {
                    any_failed = true;
                    "FAILED"
                }
                Err(_) => {
                    any_failed = true;
                    "FAILED read error"
                }
            },
            Err(_) => {
                any_failed = true;
                "FAILED open or read"
            }
        };
        if !writer.write_line(&format!("{}: {}", path, status))? {
            break;
        }
    }
    writer.flush()?;

    if !any_checked {
        bail!(
            "no valid `<hex>  <path>` entries in sumfile: {}",
            sumfile.display()
        );
    }
    Ok(if any_failed {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    })
}

/// Parse one sumfile line into `(expected_hex, path)`. Accepts the
/// `<hex>  <path>` (text) and `<hex> *<path>` (binary) shapes that
/// `shasum`/`sha256sum` emit. Blank lines and `#` comments yield `None`.
fn parse_sum_line(line: &str) -> Option<(&str, &str)> {
    let line = line.trim_end_matches(['\n', '\r']);
    let trimmed = line.trim_start();
    if trimmed.is_empty() || trimmed.starts_with('#') {
        return None;
    }
    let (hex, rest) = trimmed.split_once(char::is_whitespace)?;
    if hex.is_empty() || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    // Drop remaining separator whitespace and the binary-mode `*` marker.
    let path = rest
        .trim_start()
        .strip_prefix('*')
        .unwrap_or(rest.trim_start());
    if path.is_empty() {
        return None;
    }
    Some((hex, path))
}

/// Stream `reader` through the hasher selected by `algo`, returning lowercase
/// hex. Reads in 64KB chunks so arbitrarily large inputs never buffer whole.
fn hash_reader<R: Read>(algo: Algo, reader: R) -> io::Result<String> {
    let digest = match algo {
        Algo::Sha256 => stream_digest::<sha2::Sha256, _>(reader)?,
        Algo::Sha1 => stream_digest::<sha1::Sha1, _>(reader)?,
        Algo::Md5 => stream_digest::<md5::Md5, _>(reader)?,
        Algo::Blake3 => stream_blake3(reader)?,
    };
    Ok(to_hex(&digest))
}

/// Generic streaming hasher over any RustCrypto `Digest` (SHA-256/SHA-1/MD5).
fn stream_digest<D: Digest, R: Read>(mut reader: R) -> io::Result<Vec<u8>> {
    let mut hasher = D::new();
    let mut buf = vec![0u8; 65536];
    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hasher.finalize().to_vec())
}

/// BLAKE3 has its own hasher API (not the `Digest` trait without an extra
/// feature), so it gets a parallel streaming loop.
fn stream_blake3<R: Read>(mut reader: R) -> io::Result<Vec<u8>> {
    let mut hasher = blake3::Hasher::new();
    let mut buf = vec![0u8; 65536];
    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hasher.finalize().as_bytes().to_vec())
}

fn to_hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{:02x}", b));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn hash(algo: Algo, data: &[u8]) -> String {
        hash_reader(algo, Cursor::new(data.to_vec())).unwrap()
    }

    #[test]
    fn sha256_known_vectors() {
        assert_eq!(
            hash(Algo::Sha256, b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            hash(Algo::Sha256, b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn sha1_known_vector() {
        assert_eq!(
            hash(Algo::Sha1, b"abc"),
            "a9993e364706816aba3e25717850c26c9cd0d89d"
        );
    }

    #[test]
    fn md5_known_vector() {
        assert_eq!(hash(Algo::Md5, b"abc"), "900150983cd24fb0d6963f7d28e17f72");
    }

    #[test]
    fn blake3_known_vectors() {
        // Empty-input vector is well-known; "abc" just needs to be a stable
        // 64-hex-char digest distinct from the empty one.
        assert_eq!(
            hash(Algo::Blake3, b""),
            "af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262"
        );
        let abc = hash(Algo::Blake3, b"abc");
        assert_eq!(abc.len(), 64);
        assert_ne!(abc, hash(Algo::Blake3, b""));
    }

    #[test]
    fn streaming_matches_oneshot() {
        // A buffer larger than the 64KB chunk exercises the multi-read path.
        let big = vec![0x5au8; 200_000];
        assert_eq!(hash(Algo::Sha256, &big), {
            let mut h = sha2::Sha256::new();
            h.update(&big);
            to_hex(&h.finalize())
        });
    }

    #[test]
    fn parse_sum_line_variants() {
        assert_eq!(
            parse_sum_line("abc123  file.txt"),
            Some(("abc123", "file.txt"))
        );
        // binary-mode `*` marker
        assert_eq!(
            parse_sum_line("deadbeef *bin.dat"),
            Some(("deadbeef", "bin.dat"))
        );
        // path with spaces
        assert_eq!(
            parse_sum_line("ff  my file.txt"),
            Some(("ff", "my file.txt"))
        );
        assert_eq!(parse_sum_line(""), None);
        assert_eq!(parse_sum_line("# comment"), None);
        // non-hex first token isn't a checksum line
        assert_eq!(parse_sum_line("nothex  file"), None);
    }

    #[test]
    fn verify_roundtrip() {
        use std::io::Write;
        let dir = tempfile::tempdir().unwrap();
        let data_path = dir.path().join("data.bin");
        std::fs::write(&data_path, b"hello world").unwrap();
        let hex = hash(Algo::Sha256, b"hello world");

        // Matching sumfile → exit 0.
        let good = dir.path().join("good.sums");
        let mut f = File::create(&good).unwrap();
        writeln!(f, "{}  {}", hex, data_path.display()).unwrap();
        assert_eq!(
            verify(Algo::Sha256, &good, None).unwrap(),
            ExitCode::SUCCESS
        );

        // Wrong digest → exit 1.
        let bad = dir.path().join("bad.sums");
        let mut f = File::create(&bad).unwrap();
        writeln!(f, "{}  {}", "0".repeat(64), data_path.display()).unwrap();
        assert_eq!(verify(Algo::Sha256, &bad, None).unwrap(), ExitCode::from(1));
    }

    #[test]
    fn verify_empty_sumfile_errors() {
        let dir = tempfile::tempdir().unwrap();
        let empty = dir.path().join("empty.sums");
        std::fs::write(&empty, "# only a comment\n").unwrap();
        assert!(verify(Algo::Sha256, &empty, None).is_err());
    }
}
