//! `cert` domain — read-only inspection of X.509 certificates.
//!
//! Inputs may arrive as PEM (single cert or bundle), raw DER bytes, or PEM
//! that has been base64-wrapped (the shape Kubernetes uses for
//! `client-certificate-data` and `certificate-authority-data` fields). The
//! [`extract_ders`] helper auto-detects which form is in front of it and
//! produces a flat `Vec<Vec<u8>>` of DER blobs that the rest of the domain
//! treats uniformly.
//!
//! All commands are pure parsing — no network, no mutation surface. There is
//! deliberately no chokepoint test or read-only enforcement here: the entire
//! domain is read-only by construction.

pub mod expiring;
pub mod from_kubeconfig;
pub mod from_yaml;
pub mod hook;
pub mod inspect;

use std::io::{self, Read};
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, anyhow, bail};
use base64::Engine;
use clap::Subcommand;
use serde::Serialize;
use sha2::{Digest, Sha256};
use x509_parser::extensions::{GeneralName, ParsedExtension};
use x509_parser::pem::Pem;
use x509_parser::prelude::*;

#[derive(Subcommand)]
pub enum CertCommand {
    Inspect(inspect::InspectArgs),
    Expiring(expiring::ExpiringArgs),
    FromKubeconfig(from_kubeconfig::FromKubeconfigArgs),
    FromYaml(from_yaml::FromYamlArgs),
}

pub fn run(cmd: &CertCommand) -> Result<ExitCode> {
    match cmd {
        CertCommand::Inspect(args) => inspect::run(args),
        CertCommand::Expiring(args) => expiring::run(args),
        CertCommand::FromKubeconfig(args) => from_kubeconfig::run(args),
        CertCommand::FromYaml(args) => from_yaml::run(args),
    }
}

/// One parsed certificate, ready for emission.
///
/// Fields are owned strings so the struct can outlive the underlying DER
/// buffer it was parsed from. `days_remaining` is computed against
/// `SystemTime::now()` at parse time and may be negative for expired certs.
#[derive(Debug, Clone, Serialize)]
pub struct CertInfo {
    pub source: String,
    pub index: usize,
    pub subject: String,
    pub issuer: String,
    pub serial: String,
    pub not_before: String,
    pub not_after: String,
    pub days_remaining: i64,
    pub sans: Vec<String>,
    pub key_usage: Vec<String>,
    pub sha256_fingerprint: String,
    /// Optional context tag (e.g. kubeconfig user/cluster name) — set by
    /// commands that extract certs from structured documents. Empty for raw
    /// PEM/DER inputs.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub context: String,
}

/// Field names accepted by `--field`. Kept in one place so `inspect` and
/// `expiring` agree on the spelling.
pub const FIELD_NAMES: &[&str] = &[
    "source",
    "index",
    "subject",
    "issuer",
    "serial",
    "not_before",
    "not_after",
    "days_remaining",
    "sans",
    "key_usage",
    "sha256_fingerprint",
    "context",
];

/// Read inputs from files (or stdin), auto-detect their encoding, and return
/// a flat list of `(source-name, der-bytes)` pairs. The `source-name` is the
/// file path (or `<stdin>`); callers number multi-cert sources with the
/// returned slice index.
pub fn read_cert_inputs(files: &[PathBuf]) -> Result<Vec<(String, Vec<u8>)>> {
    let mut out = Vec::new();
    if files.is_empty() {
        let mut buf = Vec::new();
        io::stdin()
            .read_to_end(&mut buf)
            .context("error reading stdin")?;
        let ders =
            extract_ders(&buf).map_err(|e| anyhow!("no certificate found in <stdin>: {}", e))?;
        for der in ders {
            out.push(("<stdin>".to_string(), der));
        }
    } else {
        for path in files {
            let bytes =
                std::fs::read(path).with_context(|| format!("cannot read: {}", path.display()))?;
            let ders = extract_ders(&bytes)
                .map_err(|e| anyhow!("no certificate found in {}: {}", path.display(), e))?;
            for der in ders {
                out.push((path.display().to_string(), der));
            }
        }
    }
    Ok(out)
}

/// Auto-detect the encoding of `bytes` and return one DER blob per certificate.
///
/// Detection order:
///
/// 1. PEM — bytes contain at least one `-----BEGIN CERTIFICATE-----` block.
///    A bundle yields multiple DERs.
/// 2. Base64-wrapped PEM — the entire input decodes as base64 to a buffer
///    that is itself a PEM bundle (this is what `kubectl config view --raw`
///    style fields look like before the embedded `\n`s have been re-inserted).
/// 3. DER — the bytes are a single ASN.1 SEQUENCE that parses as an X.509
///    certificate.
///
/// Returns `Err` if none of the three branches succeed.
pub fn extract_ders(bytes: &[u8]) -> Result<Vec<Vec<u8>>> {
    if let Some(ders) = try_pem(bytes) {
        return Ok(ders);
    }

    if let Some(decoded) = try_base64(bytes)
        && let Some(ders) = try_pem(&decoded)
    {
        return Ok(ders);
    }

    // Try as raw DER — verify by attempting an X.509 parse. We don't keep
    // the parsed cert; the caller will re-parse from the returned bytes so
    // the lifetimes stay simple.
    if X509Certificate::from_der(bytes).is_ok() {
        return Ok(vec![bytes.to_vec()]);
    }

    bail!("input is not PEM, base64-wrapped PEM, or DER")
}

fn try_pem(bytes: &[u8]) -> Option<Vec<Vec<u8>>> {
    // Quick reject: PEM is text and must contain the CERTIFICATE header at
    // least once. This avoids paying the iterator cost on binary input.
    let needle = b"-----BEGIN CERTIFICATE-----";
    if !bytes.windows(needle.len()).any(|w| w == needle) {
        return None;
    }
    let mut ders = Vec::new();
    for pem in Pem::iter_from_buffer(bytes).flatten() {
        if pem.label == "CERTIFICATE" {
            ders.push(pem.contents);
        }
    }
    if ders.is_empty() { None } else { Some(ders) }
}

/// Try to base64-decode `bytes` after stripping ASCII whitespace. Returns
/// `None` on any non-base64 byte — this is the cheapest credible signal that
/// the input wasn't intended to be base64.
fn try_base64(bytes: &[u8]) -> Option<Vec<u8>> {
    let trimmed: Vec<u8> = bytes
        .iter()
        .copied()
        .filter(|b| !b.is_ascii_whitespace())
        .collect();
    if trimmed.is_empty() {
        return None;
    }
    base64::engine::general_purpose::STANDARD
        .decode(&trimmed)
        .ok()
}

/// Parse one DER blob into a [`CertInfo`].
pub fn parse_cert(source: &str, index: usize, der: &[u8], context: &str) -> Result<CertInfo> {
    let (_, cert) = X509Certificate::from_der(der)
        .map_err(|e| anyhow!("parsing certificate at {}#{}: {}", source, index, e))?;

    let subject = cert.subject().to_string();
    let issuer = cert.issuer().to_string();
    let serial = format_hex_colons(&cert.tbs_certificate.serial.to_bytes_be());
    let not_before_ts = cert.validity().not_before.timestamp();
    let not_after_ts = cert.validity().not_after.timestamp();
    let not_before = format_unix_iso8601(not_before_ts);
    let not_after = format_unix_iso8601(not_after_ts);
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let days_remaining = (not_after_ts - now) / 86400;

    let mut sans = Vec::new();
    let mut key_usage = Vec::new();
    for ext in cert.extensions() {
        match ext.parsed_extension() {
            ParsedExtension::SubjectAlternativeName(san) => {
                for name in &san.general_names {
                    sans.push(format_general_name(name));
                }
            }
            ParsedExtension::KeyUsage(ku) => {
                for (flag, name) in KEY_USAGE_FLAGS {
                    if ku.flags & *flag != 0 {
                        key_usage.push((*name).to_string());
                    }
                }
            }
            _ => {}
        }
    }

    let mut hasher = Sha256::new();
    hasher.update(der);
    let sha256_fingerprint = format_hex_colons(&hasher.finalize());

    Ok(CertInfo {
        source: source.to_string(),
        index,
        subject,
        issuer,
        serial,
        not_before,
        not_after,
        days_remaining,
        sans,
        key_usage,
        sha256_fingerprint,
        context: context.to_string(),
    })
}

fn format_hex_colons(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 3);
    for (i, b) in bytes.iter().enumerate() {
        if i > 0 {
            s.push(':');
        }
        s.push_str(&format!("{:02x}", b));
    }
    s
}

fn format_unix_iso8601(ts: i64) -> String {
    // RFC3339 / ISO8601 in UTC without bringing in chrono. We re-derive the
    // calendar date from a plain Unix timestamp because x509-parser's time
    // formatting is RFC2822-shaped and harder for downstream tooling to parse.
    let (year, month, day, hour, min, sec) = unix_to_utc_components(ts);
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        year, month, day, hour, min, sec
    )
}

/// Convert a Unix timestamp into `(year, month, day, hour, minute, second)` in
/// UTC. Pure integer arithmetic (Howard Hinnant's date algorithm) so we don't
/// need chrono / time formatting features just for ISO8601 emission.
fn unix_to_utc_components(secs: i64) -> (i32, u32, u32, u32, u32, u32) {
    let days_since_epoch = secs.div_euclid(86400);
    let secs_of_day = secs.rem_euclid(86400) as u32;
    let hour = secs_of_day / 3600;
    let min = (secs_of_day % 3600) / 60;
    let sec = secs_of_day % 60;

    // Days from 1970-01-01 to 0000-03-01 (Hinnant's epoch shift).
    let z = days_since_epoch + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097) as u32;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe as i32 + (era as i32) * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if month <= 2 { y + 1 } else { y };

    (year, month, day, hour, min, sec)
}

fn format_general_name(name: &GeneralName<'_>) -> String {
    match name {
        GeneralName::DNSName(s) => format!("DNS:{}", s),
        GeneralName::IPAddress(b) => match b.len() {
            4 => format!("IP:{}.{}.{}.{}", b[0], b[1], b[2], b[3]),
            16 => {
                let mut groups = Vec::with_capacity(8);
                for chunk in b.chunks(2) {
                    groups.push(format!("{:x}{:02x}", chunk[0], chunk[1]));
                }
                format!("IP:{}", groups.join(":"))
            }
            _ => format!("IP:<{} bytes>", b.len()),
        },
        GeneralName::URI(s) => format!("URI:{}", s),
        GeneralName::RFC822Name(s) => format!("email:{}", s),
        other => format!("{:?}", other),
    }
}

/// (bit-flag, openssl-style symbol) for each `keyUsage` bit. Order matches
/// RFC 5280 §4.2.1.3 so the emitted list reads naturally.
const KEY_USAGE_FLAGS: &[(u16, &str)] = &[
    (1 << 0, "digitalSignature"),
    (1 << 1, "nonRepudiation"),
    (1 << 2, "keyEncipherment"),
    (1 << 3, "dataEncipherment"),
    (1 << 4, "keyAgreement"),
    (1 << 5, "keyCertSign"),
    (1 << 6, "cRLSign"),
    (1 << 7, "encipherOnly"),
    (1 << 8, "decipherOnly"),
];

/// Output mode shared by `inspect` and `expiring`.
#[derive(Copy, Clone, Debug, PartialEq, Eq, clap::ValueEnum)]
pub enum OutputFormat {
    /// Human-readable `key<TAB>value` lines, blank-line separated per cert.
    Kv,
    /// One JSON array of cert objects.
    Json,
    /// TSV with a header row.
    Tsv,
}

/// Render a single field of one cert as a string, for `--field <name>`.
pub fn render_field(info: &CertInfo, field: &str) -> Result<String> {
    Ok(match field {
        "source" => info.source.clone(),
        "index" => info.index.to_string(),
        "subject" => info.subject.clone(),
        "issuer" => info.issuer.clone(),
        "serial" => info.serial.clone(),
        "not_before" => info.not_before.clone(),
        "not_after" => info.not_after.clone(),
        "days_remaining" => info.days_remaining.to_string(),
        "sans" => info.sans.join(","),
        "key_usage" => info.key_usage.join(","),
        "sha256_fingerprint" => info.sha256_fingerprint.clone(),
        "context" => info.context.clone(),
        other => bail!(
            "unknown --field `{}` (valid: {})",
            other,
            FIELD_NAMES.join(", ")
        ),
    })
}

/// Emit a cert as `key<TAB>value` lines via the supplied writer. Returns
/// `false` if the writer is at its limit and the caller should stop.
pub fn write_kv(writer: &mut crate::output::BoundedWriter<'_>, info: &CertInfo) -> Result<bool> {
    let pairs: [(&str, String); 12] = [
        ("source", info.source.clone()),
        ("index", info.index.to_string()),
        ("subject", info.subject.clone()),
        ("issuer", info.issuer.clone()),
        ("serial", info.serial.clone()),
        ("not_before", info.not_before.clone()),
        ("not_after", info.not_after.clone()),
        ("days_remaining", info.days_remaining.to_string()),
        ("sans", info.sans.join(",")),
        ("key_usage", info.key_usage.join(",")),
        ("sha256_fingerprint", info.sha256_fingerprint.clone()),
        ("context", info.context.clone()),
    ];
    for (k, v) in &pairs {
        // Skip empty `context` so raw PEM/DER inputs don't carry a trailing
        // empty field, but always emit every other key for grep stability.
        if *k == "context" && v.is_empty() {
            continue;
        }
        if !writer.write_line(&format!("{}\t{}", k, v))? {
            return Ok(false);
        }
    }
    Ok(true)
}

/// TSV header row matching the order used by [`write_tsv_row`].
pub const TSV_HEADER: &str = "source\tindex\tsubject\tissuer\tserial\tnot_before\tnot_after\tdays_remaining\tsans\tkey_usage\tsha256_fingerprint\tcontext";

pub fn write_tsv_row(
    writer: &mut crate::output::BoundedWriter<'_>,
    info: &CertInfo,
) -> Result<bool> {
    let row = format!(
        "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
        info.source,
        info.index,
        info.subject,
        info.issuer,
        info.serial,
        info.not_before,
        info.not_after,
        info.days_remaining,
        info.sans.join(","),
        info.key_usage.join(","),
        info.sha256_fingerprint,
        info.context,
    );
    writer.write_line(&row).map_err(Into::into)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Self-signed test cert (CN=sak-test, RSA-2048, NotBefore 2026-01-01,
    /// NotAfter 2036-01-01). Generated once with openssl and embedded so the
    /// test suite has no runtime dependency on the openssl binary or rcgen.
    /// SAN: DNS:sak-test.invalid; KeyUsage: digitalSignature, keyEncipherment.
    pub(crate) const TEST_PEM: &str = include_str!("testdata/sak-test.pem");

    #[test]
    fn extract_pem_single() {
        let ders = extract_ders(TEST_PEM.as_bytes()).unwrap();
        assert_eq!(ders.len(), 1);
    }

    #[test]
    fn extract_pem_bundle() {
        let bundle = format!("{}{}", TEST_PEM, TEST_PEM);
        let ders = extract_ders(bundle.as_bytes()).unwrap();
        assert_eq!(ders.len(), 2);
    }

    #[test]
    fn extract_base64_wrapped_pem() {
        let wrapped = base64::engine::general_purpose::STANDARD.encode(TEST_PEM);
        let ders = extract_ders(wrapped.as_bytes()).unwrap();
        assert_eq!(ders.len(), 1);
    }

    #[test]
    fn extract_raw_der() {
        let pem_ders = extract_ders(TEST_PEM.as_bytes()).unwrap();
        let ders = extract_ders(&pem_ders[0]).unwrap();
        assert_eq!(ders.len(), 1);
        assert_eq!(ders[0], pem_ders[0]);
    }

    #[test]
    fn extract_garbage_errors() {
        let err = extract_ders(b"not a certificate at all").unwrap_err();
        assert!(err.to_string().contains("not PEM"));
    }

    #[test]
    fn parse_test_cert_fields() {
        let ders = extract_ders(TEST_PEM.as_bytes()).unwrap();
        let info = parse_cert("test.pem", 0, &ders[0], "").unwrap();
        assert!(info.subject.contains("CN=sak-test"));
        assert!(info.issuer.contains("CN=sak-test"));
        assert_eq!(info.not_before, "2026-01-01T00:00:00Z");
        assert_eq!(info.not_after, "2036-01-01T00:00:00Z");
        assert!(info.sans.iter().any(|s| s == "DNS:sak-test.invalid"));
        assert!(info.key_usage.iter().any(|k| k == "digitalSignature"));
        assert!(info.key_usage.iter().any(|k| k == "keyEncipherment"));
        assert_eq!(info.sha256_fingerprint.len(), 32 * 3 - 1);
        assert!(info.sha256_fingerprint.contains(':'));
    }

    #[test]
    fn unix_to_utc_known_dates() {
        // 2026-01-01T00:00:00Z = 1767225600
        assert_eq!(unix_to_utc_components(1_767_225_600), (2026, 1, 1, 0, 0, 0));
        // 1970-01-01T00:00:00Z
        assert_eq!(unix_to_utc_components(0), (1970, 1, 1, 0, 0, 0));
        // 2000-02-29T12:34:56Z = 951827696
        assert_eq!(
            unix_to_utc_components(951_827_696),
            (2000, 2, 29, 12, 34, 56)
        );
    }

    #[test]
    fn render_field_unknown_errors() {
        let info = parse_cert(
            "test.pem",
            0,
            &extract_ders(TEST_PEM.as_bytes()).unwrap()[0],
            "",
        )
        .unwrap();
        assert!(render_field(&info, "subject").is_ok());
        assert!(render_field(&info, "no_such_field").is_err());
    }
}
