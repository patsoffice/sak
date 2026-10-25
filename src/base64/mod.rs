//! `base64` domain — encode bytes to base64, decode base64 to bytes.
//!
//! The `cert from-yaml` command already decodes base64-wrapped PEM and `sqlite
//! query` already base64-encodes BLOB columns; this domain promotes that to a
//! first-class operation so poking at k8s Secrets and JWT payloads doesn't
//! require shelling to `base64 -d` (or worse, `printf %s ... | base64 -d` with
//! newline quirks).
//!
//! Decode emits raw bytes straight to stdout (no trailing newline, mirroring
//! `base64 -d`), so `--limit` counts bytes, not lines. Encode emits text and
//! goes through [`BoundedWriter`] like every other line-oriented command, so
//! its `--limit` counts lines. Both commands always succeed on valid input
//! (even empty input decodes to empty output), mirroring the hash domain's
//! no-"no results" contract: only unreadable files and undecodable input are
//! errors.
//!
//! Pure computation — no network, no mutation surface — so like `hash` and
//! `cert` there is no chokepoint test or read-only enforcement here.

pub mod hook;

use crate::output::Outcome;
use std::io::{self, Write};
use std::path::PathBuf;

use anyhow::{Context, Result};
use base64::alphabet;
use base64::engine::{
    DecodePaddingMode, Engine, GeneralPurposeConfig,
    general_purpose::{GeneralPurpose, STANDARD, STANDARD_NO_PAD, URL_SAFE, URL_SAFE_NO_PAD},
};
use clap::{Args, Subcommand};

use crate::output::BoundedWriter;

#[derive(Subcommand)]
pub enum Base64Command {
    /// Decode base64 to raw bytes
    Decode(DecodeArgs),
    /// Encode raw bytes to base64
    Encode(EncodeArgs),
}

#[derive(Args)]
#[command(
    about = "Decode base64 to raw bytes",
    long_about = "Decode base64 input (files or stdin) and emit raw bytes to \
        stdout, with no trailing newline. By default ASCII whitespace \
        (spaces, tabs, newlines — k8s Secret data often has embedded \
        newlines) is stripped before decoding; `--strict` rejects it instead. \
        Padding is optional: `=`-padded and unpadded input both decode. \
        `--url-safe` switches to the URL-safe alphabet (JWT segments). \
        `--limit N` caps total output bytes.\n\n\
        Decoding always succeeds on valid input (even empty input, which \
        decodes to nothing); only undecodable input and unreadable files \
        are errors.",
    after_help = "\
Examples:
  sak base64 decode secret.b64                 Decode a file to stdout
  sak base64 decode --url-safe payload.txt     Decode one JWT segment (no dots)
  cat secret.b64 | sak base64 decode            Decode stdin (raw bytes, no newline)"
)]
pub struct DecodeArgs {
    /// Input files (reads stdin if omitted)
    pub files: Vec<PathBuf>,

    /// Use the URL-safe alphabet (- and _ instead of + and /)
    #[arg(long)]
    pub url_safe: bool,

    /// Reject ASCII whitespace instead of stripping it
    #[arg(long)]
    pub strict: bool,

    /// Maximum number of output bytes
    #[arg(long)]
    pub limit: Option<usize>,
}

#[derive(Args)]
#[command(
    about = "Encode raw bytes to base64",
    long_about = "Encode raw bytes (files or stdin) to base64 text. One \
        output line by default; `--wrap N` folds to N characters per line \
        (MIME style, pass 76). `--url-safe` switches to the URL-safe \
        alphabet, `--no-pad` omits the trailing `=` padding. `--limit N` \
        caps output lines.\n\n\
        Several files are concatenated and encoded as one stream, like \
        `cat a b | base64`, so the output always decodes back. Empty input \
        encodes to no output.",
    after_help = "\
Examples:
  sak base64 encode data.bin                   Encode a file
  sak base64 encode --wrap 76 data.bin         MIME-style 76-char lines
  cat data.bin | sak base64 encode              Encode stdin"
)]
pub struct EncodeArgs {
    /// Input files (reads stdin if omitted)
    pub files: Vec<PathBuf>,

    /// Use the URL-safe alphabet (- and _ instead of + and /)
    #[arg(long)]
    pub url_safe: bool,

    /// Omit trailing `=` padding
    #[arg(long)]
    pub no_pad: bool,

    /// Fold output at N characters per line (0 disables, like GNU base64)
    #[arg(long, value_name = "N")]
    pub wrap: Option<usize>,

    /// Maximum number of output lines
    #[arg(long)]
    pub limit: Option<usize>,
}

pub fn run(cmd: &Base64Command) -> Result<Outcome> {
    match cmd {
        Base64Command::Decode(args) => run_decode(args),
        Base64Command::Encode(args) => run_encode(args),
    }
}

/// Decode engine for the alphabet. Padding is indifferent: whether the `=`
/// are present can't change the decoded bytes, and requiring them only
/// rejects unpadded producers like JWT segments.
fn decode_engine(url_safe: bool) -> GeneralPurpose {
    let alpha = if url_safe {
        &alphabet::URL_SAFE
    } else {
        &alphabet::STANDARD
    };
    let config =
        GeneralPurposeConfig::new().with_decode_padding_mode(DecodePaddingMode::Indifferent);
    GeneralPurpose::new(alpha, config)
}

fn encode_engine(url_safe: bool, no_pad: bool) -> GeneralPurpose {
    match (url_safe, no_pad) {
        (false, false) => STANDARD,
        (false, true) => STANDARD_NO_PAD,
        (true, false) => URL_SAFE,
        (true, true) => URL_SAFE_NO_PAD,
    }
}

/// Decode one input. Tolerant mode strips ASCII whitespace first (k8s Secret
/// data often carries embedded newlines); strict mode decodes as-is so any
/// whitespace fails. Pure so tests can assert bytes instead of the exit
/// [`Outcome`].
pub(crate) fn decode_bytes(input: &[u8], url_safe: bool, strict: bool) -> Result<Vec<u8>> {
    let cleaned: Vec<u8> = if strict {
        input.to_vec()
    } else {
        input
            .iter()
            .copied()
            .filter(|b| !matches!(b, b' ' | b'\t' | b'\n' | b'\r' | 0x0B | 0x0C))
            .collect()
    };
    // The caller's context already says "invalid base64 in <file>".
    decode_engine(url_safe)
        .decode(&cleaned)
        .map_err(|e| anyhow::anyhow!("{e}"))
}

/// Encode one input to base64 text, folded to `wrap` columns when set.
/// A wrap of 0 disables folding, matching GNU `base64 --wrap=0`. Pure so
/// tests can assert text instead of the exit [`Outcome`].
pub(crate) fn encode_text(
    input: &[u8],
    url_safe: bool,
    no_pad: bool,
    wrap: Option<usize>,
) -> String {
    let encoded = encode_engine(url_safe, no_pad).encode(input);
    match wrap {
        Some(n) if n > 0 => {
            let bytes = encoded.as_bytes();
            let mut out = String::with_capacity(encoded.len() + encoded.len() / n);
            for (i, chunk) in bytes.chunks(n).enumerate() {
                if i > 0 {
                    out.push('\n');
                }
                // Base64 output is ASCII, so byte chunks are char boundaries.
                out.push_str(std::str::from_utf8(chunk).expect("base64 is ASCII"));
            }
            out
        }
        _ => encoded,
    }
}

fn read_inputs(files: &[PathBuf]) -> Result<Vec<(String, Vec<u8>)>> {
    let owned: Vec<PathBuf> = if files.is_empty() {
        crate::input::require_piped_stdin()?;
        vec![PathBuf::from("-")]
    } else {
        files.to_vec()
    };
    let mut out = Vec::with_capacity(owned.len());
    for path in &owned {
        out.push(crate::cert::read_path_bytes(path)?);
    }
    Ok(out)
}

fn run_decode(args: &DecodeArgs) -> Result<Outcome> {
    let inputs = read_inputs(&args.files)?;

    // Binary output bypasses BoundedWriter (line-oriented by design): the
    // limit counts raw bytes and no trailing newline is added.
    let stdout = io::stdout();
    let mut handle = stdout.lock();
    let mut remaining = args.limit;
    for (name, bytes) in &inputs {
        let decoded = decode_bytes(bytes, args.url_safe, args.strict)
            .with_context(|| format!("invalid base64 in {name}"))?;
        let chunk = match remaining {
            Some(0) => break,
            Some(n) => &decoded[..decoded.len().min(n)],
            None => &decoded[..],
        };
        handle.write_all(chunk)?;
        if let Some(n) = remaining.as_mut() {
            *n -= chunk.len();
            if *n == 0 {
                break;
            }
        }
    }
    handle.flush()?;
    Ok(Outcome::Found)
}

/// The output lines for `encode`. All inputs form one stream, like `cat a b |
/// base64`: encoding each file on its own would put padding mid-output,
/// which no decoder accepts back. Empty input yields no lines (`"".split`
/// would yield one empty line). Pure so tests can assert the lines.
fn encode_lines(inputs: Vec<(String, Vec<u8>)>, args: &EncodeArgs) -> Vec<String> {
    let joined: Vec<u8> = inputs
        .into_iter()
        .flat_map(|(_name, bytes)| bytes)
        .collect();
    let text = encode_text(&joined, args.url_safe, args.no_pad, args.wrap);
    if text.is_empty() {
        return Vec::new();
    }
    text.split('\n').map(str::to_string).collect()
}

fn run_encode(args: &EncodeArgs) -> Result<Outcome> {
    let inputs = read_inputs(&args.files)?;

    let stdout = io::stdout();
    let mut writer = BoundedWriter::new(stdout.lock(), args.limit);
    for line in encode_lines(inputs, args) {
        if !writer.write_line(&line)? {
            break;
        }
    }
    writer.flush()?;
    Ok(Outcome::Found)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_standard_vector() {
        assert_eq!(
            decode_bytes(b"aGVsbG8gd29ybGQ=", false, true).unwrap(),
            b"hello world"
        );
    }

    #[test]
    fn decode_tolerant_strips_whitespace() {
        assert_eq!(
            decode_bytes(b"aGVs\nbG8gd29y\nbGQ=\n", false, false).unwrap(),
            b"hello world"
        );
        assert_eq!(
            decode_bytes(b"  aGVsbG8=\r\n", false, false).unwrap(),
            b"hello"
        );
    }

    #[test]
    fn decode_strict_rejects_whitespace() {
        assert!(decode_bytes(b"aGVs\nbG8=", false, true).is_err());
    }

    #[test]
    fn decode_rejects_garbage() {
        assert!(decode_bytes(b"!!!", false, false).is_err());
        // One symbol carries 6 bits: not a whole byte, so never valid.
        assert!(decode_bytes(b"a", false, true).is_err());
        // Padding mid-stream (two blobs glued together) is rejected.
        assert!(decode_bytes(b"Zg==Zm8=", false, false).is_err());
    }

    #[test]
    fn decode_url_safe_alphabet() {
        // '+'/'/' in standard, '-'/'_' in URL-safe: byte 0xfb, 0xff.
        let raw = [0xfbu8, 0xffu8];
        let std = encode_text(&raw, false, false, None);
        let url = encode_text(&raw, true, false, None);
        assert!(std.contains('+') || std.contains('/'), "{std}");
        assert!(!url.contains('+') && !url.contains('/'), "{url}");
        assert_eq!(decode_bytes(url.as_bytes(), true, true).unwrap(), raw);
        // Standard alphabet rejects URL-safe input and vice versa.
        assert!(decode_bytes(url.as_bytes(), false, true).is_err());
    }

    #[test]
    fn decode_padding_is_optional() {
        // "hello" with and without its '=' padding.
        assert_eq!(decode_bytes(b"aGVsbG8", false, true).unwrap(), b"hello");
        assert_eq!(decode_bytes(b"aGVsbG8=", false, true).unwrap(), b"hello");
    }

    #[test]
    fn decode_jwt_segment() {
        // Regression: a JWT payload is unpadded URL-safe base64, and
        // `--url-safe` alone rejected it as `Invalid padding`.
        assert_eq!(
            decode_bytes(
                b"eyJzdWIiOiIxMjM0NTY3ODkwIiwibmFtZSI6IkpvaG4ifQ",
                true,
                false
            )
            .unwrap(),
            br#"{"sub":"1234567890","name":"John"}"#
        );
    }

    fn encode_args() -> EncodeArgs {
        EncodeArgs {
            files: vec![],
            url_safe: false,
            no_pad: false,
            wrap: None,
            limit: None,
        }
    }

    #[test]
    fn encode_lines_joins_inputs_into_one_stream() {
        // Regression: each file was encoded alone (`Zg==` then `Zm8=`), and
        // the padding mid-output made the result undecodable.
        let inputs = vec![
            ("a".to_string(), b"f".to_vec()),
            ("b".to_string(), b"fo".to_vec()),
        ];
        let lines = encode_lines(inputs, &encode_args());
        assert_eq!(lines, vec!["ZmZv".to_string()]);
        assert_eq!(
            decode_bytes(lines.concat().as_bytes(), false, true).unwrap(),
            b"ffo"
        );
    }

    #[test]
    fn encode_lines_empty_input_is_no_lines() {
        // Regression: empty input printed a lone newline.
        let inputs = vec![("a".to_string(), Vec::new())];
        assert!(encode_lines(inputs, &encode_args()).is_empty());
    }

    #[test]
    fn encode_known_vectors() {
        assert_eq!(encode_text(b"", false, false, None), "");
        assert_eq!(encode_text(b"f", false, false, None), "Zg==");
        assert_eq!(encode_text(b"fo", false, false, None), "Zm8=");
        assert_eq!(encode_text(b"foo", false, false, None), "Zm9v");
        assert_eq!(encode_text(b"foo", false, true, None), "Zm9v");
        assert_eq!(encode_text(b"f", false, true, None), "Zg");
    }

    #[test]
    fn encode_wrap_folds_lines() {
        // 8 encoded chars wrapped at 4 → two rows.
        assert_eq!(encode_text(b"abcdef", false, false, Some(4)), "YWJj\nZGVm");
        // Wrap wider than the output is a single line; 0 disables.
        assert_eq!(encode_text(b"abc", false, false, Some(76)), "YWJj");
        assert_eq!(encode_text(b"abcdef", false, false, Some(0)), "YWJjZGVm");
    }

    #[test]
    fn run_decode_bad_file_errors() {
        let args = DecodeArgs {
            files: vec![PathBuf::from("/nonexistent/b64.txt")],
            url_safe: false,
            strict: false,
            limit: None,
        };
        assert!(run(&Base64Command::Decode(args)).is_err());
    }

    #[test]
    fn run_decode_strict_newline_errors() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("wrapped.b64");
        std::fs::write(&p, "aGVs\nbG8=\n").unwrap();
        let args = DecodeArgs {
            files: vec![p],
            url_safe: false,
            strict: true,
            limit: None,
        };
        assert!(run(&Base64Command::Decode(args)).is_err());
    }

    #[test]
    fn run_encode_missing_file_errors() {
        let args = EncodeArgs {
            files: vec![PathBuf::from("/nonexistent/data.bin")],
            url_safe: false,
            no_pad: false,
            wrap: None,
            limit: None,
        };
        assert!(run(&Base64Command::Encode(args)).is_err());
    }
}
