use crate::output::Outcome;
use std::io;
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use clap::{Args, ValueEnum};
use oid_registry::{
    OID_PKCS1_RSAENCRYPTION, OID_PKCS1_SHA1WITHRSA, OID_PKCS1_SHA256WITHRSA,
    OID_PKCS1_SHA384WITHRSA, OID_PKCS1_SHA512WITHRSA, OID_SHA1_WITH_RSA, Oid,
};
use serde::Serialize;
use x509_parser::extensions::ParsedExtension;
use x509_parser::prelude::*;

use crate::cert::{extract_ders, read_path_bytes};
use crate::output::BoundedWriter;

/// Output mode for `verify-chain`.
#[derive(Copy, Clone, Debug, PartialEq, Eq, ValueEnum)]
pub enum ChainFormat {
    /// `chain<TAB>n<TAB>length=L<TAB>valid|invalid` plus one indented row
    /// per cert and per finding.
    Kv,
    /// One JSON array of chain objects.
    Json,
}

#[derive(Args)]
#[command(
    about = "Verify X.509 certificate chains",
    long_about = "Verify X.509 certificate chains: check that each chain runs leaf \
        first and ends in a self-signed root, that every cert's issuer \
        matches the next cert's subject (names compare ignoring string type, \
        case, and extra spaces, like openssl), that every signature verifies \
        against the issuer's public key, that every cert is within its \
        validity window, and that every issuer carries `CA:TRUE` basic \
        constraints and respects any path-length limit.\n\n\
        Signatures are checked with ring's supported set: RSA PKCS#1 with \
        2048–8192-bit keys (1024-bit floor for SHA-1), ECDSA P-256/P-384, \
        Ed25519. Anything else records a `can't check` note instead of a \
        verdict: a chain with no definite problem but such a note is \
        reported `unverified`, while any chain with a definite finding is \
        `invalid` regardless of notes. A self-signed root with no \
        basicConstraints is still accepted when it is an X.509 v1 cert or \
        its keyUsage includes keyCertSign, matching openssl; any issuer \
        whose keyUsage omits keyCertSign is invalid, trust anchors \
        included.\n\n\
        Inputs may be PEM (single cert or bundle), raw DER, or base64-wrapped \
        PEM — the same auto-detection as `inspect`.\n\n\
        Bundle mode (no `--ca`): each input file (or stdin) is one complete \
        chain; certs are linked structurally so order within the file doesn't \
        matter, and certs in different files are never linked.\n\n\
        Anchored mode (`--ca`, like `openssl verify -CAfile`): the first cert \
        of each input is the cert to verify. Its chain is built from the rest \
        of that input, the `--untrusted` intermediates, and the `--ca` certs, \
        and must end at a self-signed `--ca` cert.\n\n\
        Nothing is checked against the system trust store: plain `openssl \
        verify cert.pem` has no equivalent here.\n\n\
        Exit 0 means every chain is valid, exit 1 means at least one chain \
        is invalid, exit 2 is a tool error: unparsable input, bad `--at`, \
        or a chain that is unverified rather than invalid.",
    after_help = "\
Examples:
  sak cert verify-chain chain.pem                 Check a leaf+inter+root bundle
  sak cert verify-chain --ca root.pem --untrusted inter.pem leaf.pem
                                                  Like openssl verify -CAfile root.pem -untrusted inter.pem leaf.pem
  sak cert verify-chain --ca ca-bundle.pem server.pem
                                                  Server cert (plus any intermediates it carries) against a CA bundle
  sak cert verify-chain --at 2030-01-01T00:00:00Z chain.pem   Validity at an instant
  sak cert verify-chain --allow-self-signed cert.pem   Accept a lone self-signed cert
  sak cert verify-chain --json chain.pem          Structured output for piping"
)]
pub struct VerifyChainArgs {
    /// Input files (reads stdin if omitted, or for a `-` argument)
    pub files: Vec<PathBuf>,

    /// Trust anchors, like `openssl verify -CAfile` (repeatable; bundles
    /// fine). Each input's first cert is then verified, and its chain must
    /// end at one of these
    #[arg(long, value_name = "FILE")]
    pub ca: Vec<PathBuf>,

    /// Intermediates to build chains from, like `openssl verify -untrusted`
    /// (repeatable; needs --ca)
    #[arg(long, value_name = "FILE", requires = "ca")]
    pub untrusted: Vec<PathBuf>,

    /// Output format (default: kv)
    #[arg(long, value_enum, default_value_t = ChainFormat::Kv)]
    pub format: ChainFormat,

    /// Convenience for --format json
    #[arg(long, conflicts_with = "format")]
    pub json: bool,

    /// Verify validity at this instant (RFC3339, e.g. 2030-01-01T00:00:00Z)
    /// instead of now
    #[arg(long, value_name = "RFC3339")]
    pub at: Option<String>,

    /// Accept a single self-signed cert as a valid one-cert chain (bundle
    /// mode; with --ca, put the cert in the CA file instead)
    #[arg(long, conflicts_with = "ca")]
    pub allow_self_signed: bool,

    /// Maximum number of output lines
    #[arg(long)]
    pub limit: Option<usize>,
}

/// One verified chain: the assembled cert order plus every finding.
/// `uncertain` holds "can't check" notes for signatures the backend cannot
/// evaluate. `valid` means fully verified: no definite `issues` and nothing
/// unevaluated. A chain with definite issues is invalid (exit 1); a chain
/// with no definite issues but uncertain notes is unverified (exit 2).
#[derive(Debug, Clone, Serialize)]
pub struct ChainReport {
    pub chain: usize,
    pub source: String,
    pub length: usize,
    pub valid: bool,
    pub certs: Vec<ChainCert>,
    pub issues: Vec<String>,
    pub uncertain: Vec<String>,
}

/// One cert in chain order, with the display role from the spec.
#[derive(Debug, Clone, Serialize)]
pub struct ChainCert {
    pub role: String,
    pub subject: String,
    pub issuer: String,
    pub serial: String,
    pub not_before: String,
    pub not_after: String,
}

/// RFC 5280 name comparison: attribute types must match, but values compare
/// ignoring string type, ASCII case, and surrounding/extra whitespace. The
/// derived `PartialEq` on `X509Name` compares raw DER, so a PrintableString
/// issuer and a UTF8String subject holding the same text would falsely
/// mismatch.
fn names_equal(a: &X509Name, b: &X509Name) -> bool {
    fn key(name: &X509Name) -> Vec<(String, Vec<u8>)> {
        name.iter_attributes()
            .map(|attr| {
                let oid = attr.attr_type().to_string();
                // Only the common string types normalize; anything exotic
                // (BMPString, …) compares as raw bytes, like before.
                // Case folds ASCII only, like openssl.
                let value = match attr.as_str() {
                    Ok(s) => s
                        .split_whitespace()
                        .collect::<Vec<_>>()
                        .join(" ")
                        .to_ascii_lowercase()
                        .into_bytes(),
                    Err(_) => attr.as_slice().to_vec(),
                };
                (oid, value)
            })
            .collect()
    }
    key(a) == key(b)
}

/// A signature the backend cannot evaluate is a tool error (exit 2), never
/// a verdict: reporting "invalid" would contradict `openssl verify` on
/// chains that are actually fine (RSA-PSS, P-521, ecdsa-with-SHA512, …).
fn unsupported(err: &X509Error) -> bool {
    matches!(err, X509Error::SignatureUnsupportedAlgorithm)
}

/// The outcome of one signature verification.
enum SigCheck {
    /// Verifies against the issuer's key.
    Good,
    /// Does not verify: a definite finding.
    Bad,
    /// Cannot evaluate: the note goes to the chain's `uncertain` list.
    Unknown(String),
}

/// Verify a signature, distinguishing "bad" from "cannot evaluate". Ring
/// only handles RSA keys of 2048–8192 bits (1024–8192 for SHA-1), so an
/// out-of-range RSA key is detected up front: ring reports those as a
/// generic failure, indistinguishable from a genuinely bad signature.
fn check_sig(
    result: Result<(), X509Error>,
    issuer_key: Option<&SubjectPublicKeyInfo>,
    sig_alg: &Oid,
    what: &str,
) -> SigCheck {
    match result {
        Ok(()) => SigCheck::Good,
        Err(e) if unsupported(&e) => SigCheck::Unknown(format!(
            "can't check {what} signature: algorithm not supported"
        )),
        Err(_) => match rsa_modulus_bits(issuer_key) {
            Some(bits) if !rsa_range_ok(sig_alg, bits) => SigCheck::Unknown(format!(
                "can't check {what} signature: RSA {bits}-bit key outside ring's range"
            )),
            _ => SigCheck::Bad,
        },
    }
}

/// Ring's RSA key-size range for a signature algorithm: 1024–8192 bits for
/// SHA-1, 2048–8192 for SHA-256/384/512. Anything else is not an RSA
/// signature ring would evaluate, so no pre-check applies. Like ring
/// (`rsa/public_modulus.rs`), the modulus size rounds up to whole bytes
/// before comparing: a 2041-bit key occupies 256 bytes and evaluates.
fn rsa_range_ok(sig_alg: &Oid, bits: u32) -> bool {
    let min = if *sig_alg == OID_PKCS1_SHA1WITHRSA || *sig_alg == OID_SHA1_WITH_RSA {
        1024
    } else if *sig_alg == OID_PKCS1_SHA256WITHRSA
        || *sig_alg == OID_PKCS1_SHA384WITHRSA
        || *sig_alg == OID_PKCS1_SHA512WITHRSA
    {
        2048
    } else {
        return true;
    };
    let effective = bits.div_ceil(8) * 8;
    effective >= min && effective <= 8192
}

/// RSA public-key size in bits, or `None` when the key isn't RSA or the
/// inner DER doesn't parse (in which case ring's own verdict stands).
fn rsa_modulus_bits(spki: Option<&SubjectPublicKeyInfo>) -> Option<u32> {
    let spki = spki?;
    if spki.algorithm.algorithm != OID_PKCS1_RSAENCRYPTION {
        return None;
    }
    let b: &[u8] = spki.subject_public_key.data.as_ref();
    let mut pos = 0;
    if *b.get(pos)? != 0x30 {
        return None;
    }
    pos += 1;
    read_der_len(b, &mut pos)?; // SEQUENCE length
    if *b.get(pos)? != 0x02 {
        return None;
    }
    pos += 1;
    let int_len = read_der_len(b, &mut pos)?;
    let mut m = b.get(pos..pos + int_len)?;
    if m.first() == Some(&0) {
        m = &m[1..]; // leading zero pad, not part of the modulus
    }
    let top = *m.first()?;
    Some((m.len() as u32 - 1) * 8 + (8 - top.leading_zeros()))
}

/// Read one ASN.1 length at `pos`, advancing past it.
fn read_der_len(b: &[u8], pos: &mut usize) -> Option<usize> {
    let first = *b.get(*pos)?;
    *pos += 1;
    if first & 0x80 == 0 {
        return Some(first as usize);
    }
    let n = (first & 0x7f) as usize;
    if n == 0 || n > 4 {
        return None;
    }
    let mut len = 0usize;
    for _ in 0..n {
        len = (len << 8) | (*b.get(*pos)? as usize);
        *pos += 1;
    }
    Some(len)
}

pub fn run(args: &VerifyChainArgs) -> Result<Outcome> {
    let format = if args.json {
        ChainFormat::Json
    } else {
        args.format
    };

    let at = match &args.at {
        Some(s) => Some(
            parse_rfc3339(s)
                .with_context(|| format!("invalid --at timestamp `{s}` (want RFC3339)"))?,
        ),
        None => None,
    };
    let now = at.unwrap_or_else(unix_now);

    // Each input source is one chain. Certs keep their DER bytes so the
    // borrow checker stays happy: parse borrows from the stored buffer.
    let mut sources: Vec<(String, Vec<Vec<u8>>)> = Vec::new();
    if args.files.is_empty() {
        crate::input::require_piped_stdin()?;
        sources.push(read_ders(std::path::Path::new("-"))?);
    } else {
        for path in &args.files {
            sources.push(read_ders(path)?);
        }
    }
    // Anchored mode pools: every --ca and --untrusted file, keeping each
    // file's name so a bad cert is reported at its own file, not the leaf's.
    let mut anchors = Vec::new();
    for path in &args.ca {
        anchors.push(read_ders(path)?);
    }
    let mut untrusted = Vec::new();
    for path in &args.untrusted {
        untrusted.push(read_ders(path)?);
    }

    let mut reports = Vec::with_capacity(sources.len());
    for (i, (source, ders)) in sources.iter().enumerate() {
        reports.push(if anchors.is_empty() {
            verify_one(i + 1, source, ders, now, args.allow_self_signed)?
        } else {
            verify_anchored(i + 1, source, ders, &untrusted, &anchors, now)?
        });
    }

    // A definite problem anywhere means invalid (exit 1), even when other
    // chains only carry "can't check" notes. Otherwise any "can't check"
    // is a tool error (exit 2), reported after the full output.
    let outcome = emit(&reports, format, args.limit)?;
    if outcome == Outcome::NotFound {
        return Ok(outcome);
    }
    let stuck: Vec<&str> = reports
        .iter()
        .flat_map(|r| r.uncertain.iter().map(String::as_str))
        .collect();
    if stuck.is_empty() {
        return Ok(outcome);
    }
    Err(anyhow::anyhow!("{}", stuck.join("; ")))
}

/// Read one input (`-` is stdin) as DER certs, failing when it holds none.
fn read_ders(path: &std::path::Path) -> Result<(String, Vec<Vec<u8>>)> {
    let (name, bytes) = read_path_bytes(path)?;
    let ders =
        extract_ders(&bytes).map_err(|e| anyhow::anyhow!("no certificate found in {name}: {e}"))?;
    Ok((name, ders))
}

/// Parse one DER cert, naming the source and position of a failure.
fn parse_one<'a>(source: &str, idx: usize, der: &'a [u8]) -> Result<X509Certificate<'a>> {
    X509Certificate::from_der(der)
        .map(|(_, cert)| cert)
        .map_err(|e| anyhow::anyhow!("parsing certificate at {source}#{idx}: {e}"))
}

/// Parse DER certs, naming the source and position of any that fail.
fn parse_all<'a>(source: &str, ders: &[&'a [u8]]) -> Result<Vec<X509Certificate<'a>>> {
    ders.iter()
        .enumerate()
        .map(|(i, der)| parse_one(source, i, der))
        .collect()
}

/// Bundle mode: verify one input source as a complete chain.
fn verify_one(
    num: usize,
    source: &str,
    ders: &[Vec<u8>],
    now: i64,
    allow_self_signed: bool,
) -> Result<ChainReport> {
    let ders: Vec<&[u8]> = ders.iter().map(Vec::as_slice).collect();
    let parsed = parse_all(source, &ders)?;
    // Assemble chain order structurally: the leaf is the cert whose subject
    // is nobody's issuer, then follow issuer→subject links. File order is
    // only the fallback when linkage is ambiguous (e.g. every cert links,
    // or several leaves).
    let order = assemble_order(&parsed);
    check_chain(
        num,
        source,
        &parsed,
        &ders,
        &order,
        now,
        allow_self_signed,
        None,
    )
}

/// Anchored mode (`--ca`): verify the first cert of `target` with a chain
/// built from the rest of `target`, the `untrusted` pool, and the `anchors`,
/// which must end at one of the anchors. Each pool keeps its own file names
/// so a bad cert is reported at its own file, not the leaf's.
fn verify_anchored(
    num: usize,
    source: &str,
    target: &[Vec<u8>],
    untrusted: &[(String, Vec<Vec<u8>>)],
    anchors: &[(String, Vec<Vec<u8>>)],
    now: i64,
) -> Result<ChainReport> {
    let mut ders: Vec<&[u8]> = Vec::new();
    let mut parsed: Vec<X509Certificate<'_>> = Vec::new();
    for (i, der) in target.iter().enumerate() {
        ders.push(der);
        parsed.push(parse_one(source, i, der)?);
    }
    for (name, file) in untrusted {
        for (i, der) in file.iter().enumerate() {
            ders.push(der);
            parsed.push(parse_one(name, i, der)?);
        }
    }
    let anchor_from = ders.len();
    for (name, file) in anchors {
        for (i, der) in file.iter().enumerate() {
            ders.push(der);
            parsed.push(parse_one(name, i, der)?);
        }
    }
    let order = build_from(&parsed, 0, anchor_from);
    let anchor_ders: Vec<&[u8]> = ders[anchor_from..].to_vec();
    check_chain(
        num,
        source,
        &parsed,
        &ders,
        &order,
        now,
        false,
        Some(&anchor_ders),
    )
}

/// Run every check over `parsed` in chain `order` (leaf first). `ders` is
/// parallel to `parsed`. With `anchors`, the chain must also end at one of
/// those certs (matched by DER bytes), and a lone cert is judged like any
/// chain instead of by the `--allow-self-signed` rule.
#[allow(clippy::too_many_arguments)]
fn check_chain(
    num: usize,
    source: &str,
    parsed: &[X509Certificate<'_>],
    ders: &[&[u8]],
    order: &[usize],
    now: i64,
    allow_self_signed: bool,
    anchors: Option<&[&[u8]]>,
) -> Result<ChainReport> {
    let n = order.len();

    let mut issues = Vec::new();
    let mut uncertain = Vec::new();
    let mut cert_infos = Vec::with_capacity(n);
    for (pos, &idx) in order.iter().enumerate() {
        let cert = &parsed[idx];
        // The last cert is a root only when it is self-signed: an
        // incomplete chain ends in a cert whose issuer is absent (an
        // issuer in passing, or a lone leaf), and calling it a root
        // mislabels the finding.
        let role = if pos == n - 1 && names_equal(cert.subject(), cert.issuer()) {
            "root"
        } else if pos == 0 {
            "leaf"
        } else {
            "inter"
        };
        cert_infos.push(ChainCert {
            role: role.to_string(),
            subject: cert.subject().to_string(),
            issuer: cert.issuer().to_string(),
            serial: format_serial(&cert.tbs_certificate.serial.to_bytes_be()),
            not_before: format_unix_iso8601(cert.validity().not_before.timestamp()),
            not_after: format_unix_iso8601(cert.validity().not_after.timestamp()),
        });
    }

    if n == 1 && anchors.is_none() {
        verify_single(
            &parsed[order[0]],
            now,
            allow_self_signed,
            &mut issues,
            &mut uncertain,
        );
        return Ok(report(num, source, cert_infos, issues, uncertain));
    }

    for (pos, window) in order.windows(2).enumerate() {
        let (child, issuer) = (&parsed[window[0]], &parsed[window[1]]);
        let child_no = pos + 1;
        let issuer_no = pos + 2;
        // 2. Subject/issuer linkage: checked before signatures so an
        // unevaluatable signature never hides a definite mismatch.
        if !names_equal(child.issuer(), issuer.subject()) {
            issues.push(format!(
                "cert {child_no} issuer ({}) does not match cert {issuer_no} subject ({})",
                child.issuer(),
                issuer.subject(),
            ));
            // Without linkage the signature check below would blame the
            // wrong key, so skip it for this pair.
            continue;
        }
        // 3. Signature verification against the issuer's public key. An
        // unevaluatable signature is recorded, not reported: checking
        // continues so definite findings still surface.
        match check_sig(
            child.verify_signature(Some(issuer.public_key())),
            Some(issuer.public_key()),
            &child.signature_algorithm.algorithm,
            &format!("cert {child_no}"),
        ) {
            SigCheck::Good => {}
            SigCheck::Bad => issues.push(format!(
                "cert {child_no} signature does not verify against cert {issuer_no} public key"
            )),
            SigCheck::Unknown(note) => uncertain.push(note),
        }
        // 5. CA checks on the issuer side of this pair (never the leaf).
        // `pos` doubles as the count of non-leaf certs below the issuer:
        // it sits at chain position pos+1 with positions 0..=pos beneath,
        // of which only position 0 is the leaf.
        let is_self_signed_root = issuer_no == n && names_equal(issuer.subject(), issuer.issuer());
        check_ca_constraint(issuer, issuer_no, pos, is_self_signed_root, &mut issues);
    }

    // 1. The chain must end in a root (self-signed) cert — with --ca, one
    // of the trust anchors.
    let last = &parsed[order[n - 1]];
    if names_equal(last.subject(), last.issuer()) {
        let is_anchor = anchors.is_some_and(|a| a.contains(&ders[order[n - 1]]));
        // A byte-matched trust anchor is trusted as-is: like openssl, no
        // self-signature check (it may use an algorithm sak can't verify).
        if !is_anchor {
            match check_sig(
                last.verify_signature(None),
                Some(last.public_key()),
                &last.signature_algorithm.algorithm,
                &format!("root cert {n}"),
            ) {
                SigCheck::Good => {}
                SigCheck::Bad => {
                    issues.push(format!("root cert {n} self-signature does not verify"))
                }
                SigCheck::Unknown(note) => uncertain.push(note),
            }
        }
        if let Some(anchors) = anchors
            && !anchors.contains(&ders[order[n - 1]])
        {
            issues.push(format!(
                "root cert {n} ({}) is not one of the --ca certificates",
                last.subject()
            ));
        }
    } else {
        issues.push(format!(
            "chain ends in non-self-signed cert {n} ({}); issuer not present",
            last.subject()
        ));
    }

    // 4. Every cert must be within its validity window at the instant.
    for (pos, &idx) in order.iter().enumerate() {
        let cert = &parsed[idx];
        let nb = cert.validity().not_before.timestamp();
        let na = cert.validity().not_after.timestamp();
        if now < nb {
            issues.push(format!(
                "cert {} not yet valid at {} (NotBefore {})",
                pos + 1,
                format_unix_iso8601(now),
                format_unix_iso8601(nb),
            ));
        } else if now > na {
            issues.push(format!(
                "cert {} expired at {} (NotAfter {})",
                pos + 1,
                format_unix_iso8601(now),
                format_unix_iso8601(na),
            ));
        }
    }

    Ok(report(num, source, cert_infos, issues, uncertain))
}

/// Single-cert chain: valid only with `--allow-self-signed`, and only when
/// the cert is actually self-signed with a verifying self-signature inside
/// its validity window.
fn verify_single(
    cert: &X509Certificate<'_>,
    now: i64,
    allow_self_signed: bool,
    issues: &mut Vec<String>,
    uncertain: &mut Vec<String>,
) {
    if !names_equal(cert.subject(), cert.issuer()) {
        issues.push(format!(
            "single-cert chain is not self-signed (subject {}, issuer {})",
            cert.subject(),
            cert.issuer(),
        ));
        return;
    }
    if !allow_self_signed {
        issues.push(
            "single self-signed cert needs --allow-self-signed to count as a chain".to_string(),
        );
        return;
    }
    match check_sig(
        cert.verify_signature(None),
        Some(cert.public_key()),
        &cert.signature_algorithm.algorithm,
        "self",
    ) {
        SigCheck::Good => {}
        SigCheck::Bad => issues.push("self-signature does not verify".to_string()),
        SigCheck::Unknown(note) => uncertain.push(note),
    }
    let nb = cert.validity().not_before.timestamp();
    let na = cert.validity().not_after.timestamp();
    if now < nb || now > na {
        issues.push(format!(
            "cert not valid at {} (window {} to {})",
            format_unix_iso8601(now),
            format_unix_iso8601(nb),
            format_unix_iso8601(na),
        ));
    }
}

/// Check `CA:TRUE` (and any path-length constraint) on a non-leaf cert.
/// `governed` is the number of non-leaf certs below it in the chain (the
/// leaf at position 0 never counts). Like openssl, any issuer whose
/// keyUsage omits keyCertSign is invalid — trust anchors included. A
/// self-signed root with no basicConstraints is still accepted when it is
/// an X.509 v1 cert or its keyUsage includes keyCertSign.
fn check_ca_constraint(
    issuer: &X509Certificate<'_>,
    issuer_no: usize,
    governed: usize,
    is_self_signed_root: bool,
    issues: &mut Vec<String>,
) {
    let mut found_bc = false;
    let mut ca_true = false;
    let mut path_len: Option<u32> = None;
    let mut found_ku = false;
    let mut ku_sign = false;
    for ext in issuer.extensions() {
        match ext.parsed_extension() {
            ParsedExtension::BasicConstraints(bc) => {
                found_bc = true;
                ca_true = bc.ca;
                path_len = bc.path_len_constraint;
            }
            ParsedExtension::KeyUsage(ku) => {
                found_ku = true;
                if ku.key_cert_sign() {
                    ku_sign = true;
                }
            }
            _ => {}
        }
    }
    if found_bc {
        if !ca_true {
            issues.push(format!(
                "cert {issuer_no} ({}) is an issuer but has CA:FALSE",
                issuer.subject()
            ));
        } else if let Some(max) = path_len
            && governed as u32 > max
        {
            issues.push(format!(
                "cert {issuer_no} path length constraint violated (allows {max}, {governed} CA certs below)"
            ));
        }
    } else if !(is_self_signed_root && (issuer.version() == X509Version::V1 || ku_sign)) {
        issues.push(format!(
            "cert {issuer_no} ({}) is an issuer but has no basicConstraints",
            issuer.subject()
        ));
    }
    if found_ku && !ku_sign {
        issues.push(format!(
            "cert {issuer_no} ({}) keyUsage does not include keyCertSign",
            issuer.subject()
        ));
    }
}

/// Anchored-mode chain order: start at `start` (the cert being verified) and
/// follow issuer links through every other cert, stopping at a self-signed
/// cert or when no issuer is present. Among certs whose subject matches,
/// prefer one whose key verifies the signature — a CA bundle can hold
/// several certs with one name (re-keyed or cross-signed CAs). Trust
/// anchors (`parsed[anchor_from..]`) sort before the rest: a served bundle
/// can carry a cross-signed copy of a CA (same name and key, issued by an
/// absent older root) that verifies signatures just like the anchor but
/// ends the chain nowhere, so the anchor must win — mirroring
/// `openssl verify`, which consults trusted certs before untrusted ones.
fn build_from(certs: &[X509Certificate<'_>], start: usize, anchor_from: usize) -> Vec<usize> {
    let mut order = vec![start];
    loop {
        let cur = &certs[*order.last().expect("order starts non-empty")];
        if names_equal(cur.subject(), cur.issuer()) {
            return order;
        }
        let mut candidates: Vec<usize> = (0..certs.len())
            .filter(|&i| !order.contains(&i) && names_equal(certs[i].subject(), cur.issuer()))
            .collect();
        candidates.sort_by_key(|&i| usize::from(i < anchor_from));
        let next = candidates
            .iter()
            .copied()
            .find(|&i| cur.verify_signature(Some(certs[i].public_key())).is_ok())
            .or_else(|| candidates.first().copied());
        match next {
            Some(i) => order.push(i),
            None => return order,
        }
    }
}

/// Order cert indices leaf-first by following issuer→subject links. Falls
/// back to file order when linkage is ambiguous.
fn assemble_order(certs: &[X509Certificate<'_>]) -> Vec<usize> {
    if certs.len() <= 1 {
        return (0..certs.len()).collect();
    }
    // A leaf is a cert whose subject is nobody else's issuer.
    let leaves: Vec<usize> = (0..certs.len())
        .filter(|&i| {
            !certs
                .iter()
                .enumerate()
                .any(|(j, other)| j != i && names_equal(other.issuer(), certs[i].subject()))
        })
        .collect();
    if leaves.len() != 1 {
        return (0..certs.len()).collect();
    }
    let mut order = vec![leaves[0]];
    while order.len() < certs.len() {
        let cur = &certs[*order.last().unwrap()];
        // Next is the cert (not already placed) whose subject matches,
        // preferring one whose key verifies the signature: a bundle can
        // hold two versions of a CA with the same name but different keys,
        // and file order alone may pick the wrong one.
        let cands: Vec<usize> = (0..certs.len())
            .filter(|&i| !order.contains(&i) && names_equal(certs[i].subject(), cur.issuer()))
            .collect();
        let next = cands
            .iter()
            .copied()
            .find(|&i| cur.verify_signature(Some(certs[i].public_key())).is_ok())
            .or_else(|| cands.first().copied());
        match next {
            Some(i) => order.push(i),
            None => {
                // Broken link: append the rest in file order so every cert
                // still gets reported (the linkage issue is flagged later).
                let rest: Vec<usize> = (0..certs.len()).filter(|i| !order.contains(i)).collect();
                order.extend(rest);
            }
        }
    }
    order
}

/// Display status for a chain: `valid` means fully verified, `invalid`
/// means a definite finding, `unverified` means no definite finding but
/// something the backend could not evaluate.
fn chain_status(report: &ChainReport) -> &'static str {
    if !report.issues.is_empty() {
        "invalid"
    } else if !report.uncertain.is_empty() {
        "unverified"
    } else {
        "valid"
    }
}

fn report(
    num: usize,
    source: &str,
    certs: Vec<ChainCert>,
    issues: Vec<String>,
    uncertain: Vec<String>,
) -> ChainReport {
    let valid = issues.is_empty() && uncertain.is_empty();
    ChainReport {
        chain: num,
        source: source.to_string(),
        length: certs.len(),
        valid,
        certs,
        issues,
        uncertain,
    }
}

fn emit(reports: &[ChainReport], format: ChainFormat, limit: Option<usize>) -> Result<Outcome> {
    let stdout = io::stdout();
    let handle = stdout.lock();
    let mut writer = BoundedWriter::new(handle, limit);

    // A definite finding anywhere means exit 1, even when other chains
    // only carry "can't check" notes. The caller maps the remaining
    // all-clear-or-unverified case to exit 0 or a tool error.
    let any_invalid = reports.iter().any(|r| !r.issues.is_empty());
    if format == ChainFormat::Json {
        let pretty = serde_json::to_string_pretty(reports)?;
        writer.write_decoration(&pretty)?;
        writer.flush()?;
        return Ok(if any_invalid {
            Outcome::NotFound
        } else {
            Outcome::Found
        });
    }

    for r in reports {
        let status = chain_status(r);
        if !writer.write_line(&format!(
            "chain\t{}\tlength={}\t{status}",
            r.chain, r.length
        ))? {
            break;
        }
        for c in &r.certs {
            if !writer.write_line(&format!("  {}\t{}\t{}", c.role, c.subject, c.not_after))? {
                break;
            }
        }
        for issue in &r.issues {
            if !writer.write_line(&format!("  issue\t{issue}"))? {
                break;
            }
        }
        for note in &r.uncertain {
            if !writer.write_line(&format!("  uncertain\t{note}"))? {
                break;
            }
        }
    }

    writer.flush()?;
    Ok(if any_invalid {
        Outcome::NotFound
    } else {
        Outcome::Found
    })
}

fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn format_serial(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<Vec<_>>()
        .join(":")
}

fn format_unix_iso8601(ts: i64) -> String {
    let (year, month, day, hour, min, sec) = crate::cert::unix_to_utc_components(ts);
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{min:02}:{sec:02}Z")
}

/// Parse an RFC3339 instant (`YYYY-MM-DDTHH:MM:SSZ` or with a `±HH:MM`
/// offset) into a Unix timestamp. Hand-rolled so the cert domain keeps no
/// date-time dependency; deliberately strict (UTC `Z` plus numeric offsets
/// only, no fractional seconds, no leap-second 60).
fn parse_rfc3339(s: &str) -> Result<i64> {
    // Every numeric field must be plain ASCII digits: `i64::parse` would
    // otherwise accept a leading `+`/`-`, letting `-1` through as an hour.
    fn digits(part: &str, what: &str) -> Result<i64> {
        if !part.bytes().all(|b| b.is_ascii_digit()) {
            bail!("bad {what} (want plain digits)");
        }
        part.parse::<i64>()
            .map_err(|e| anyhow::anyhow!("bad {what}: {e}"))
    }
    let (dt, offset_secs) = match s.strip_suffix('Z').or_else(|| s.strip_suffix('z')) {
        Some(dt) => (dt, 0i64),
        None => {
            let (pos, sign) = s
                .rfind('+')
                .map(|p| (p, 1i64))
                .or_else(|| s.rfind('-').filter(|&p| p > 9).map(|p| (p, -1i64)))
                .ok_or_else(|| anyhow::anyhow!("want trailing Z or ±HH:MM offset"))?;
            let (dt, off) = (&s[..pos], &s[pos + 1..]);
            let (hh, mm) = off
                .split_once(':')
                .ok_or_else(|| anyhow::anyhow!("bad offset `{off}` (want ±HH:MM)"))?;
            if hh.len() != 2 || mm.len() != 2 {
                bail!("bad offset `{off}` (want ±HH:MM)");
            }
            let (oh, om) = (digits(hh, "offset hour")?, digits(mm, "offset minute")?);
            if oh > 23 || om > 59 {
                bail!("bad offset `{off}` (want ±HH:MM)");
            }
            (dt, sign * (oh * 3600 + om * 60))
        }
    };
    let (date, time) = dt
        .split_once('T')
        .ok_or_else(|| anyhow::anyhow!("want `T` between date and time"))?;
    let d: Vec<&str> = date.split('-').collect();
    let t: Vec<&str> = time.split(':').collect();
    if d.len() != 3 || t.len() != 3 {
        bail!("want YYYY-MM-DDTHH:MM:SS");
    }
    if d[0].len() != 4 || d[1].len() != 2 || d[2].len() != 2 {
        bail!("bad date `{date}` (want YYYY-MM-DD)");
    }
    for (part, name) in [(t[0], "hour"), (t[1], "minute"), (t[2], "second")] {
        if part.len() != 2 {
            bail!("bad {name} in `{time}` (want HH:MM:SS)");
        }
    }
    let (y, mo, dd): (i64, i64, i64) = (
        digits(d[0], "year")?,
        digits(d[1], "month")?,
        digits(d[2], "day")?,
    );
    let (hh, mm, ss): (i64, i64, i64) = (
        digits(t[0], "hour")?,
        digits(t[1], "minute")?,
        digits(t[2], "second")?,
    );
    if !(1..=12).contains(&mo) || dd < 1 {
        bail!("date out of range");
    }
    // Gregorian days-in-month, leap years included: the parser is
    // deliberately strict, so 04-31 and non-leap 02-29 fail instead of
    // rolling into the next month.
    let leap = (y % 4 == 0 && y % 100 != 0) || y % 400 == 0;
    let dim = match mo {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if leap => 29,
        _ => 28,
    };
    if dd > dim {
        bail!("date out of range");
    }
    if hh > 23 || mm > 59 || ss > 59 {
        bail!("time out of range");
    }
    // Howard Hinnant's days-from-civil.
    let y2 = if mo <= 2 { y - 1 } else { y };
    let era = y2.div_euclid(400);
    let yoe = y2.rem_euclid(400);
    let mp = (mo + 9) % 12;
    let doy = (153 * mp + 2) / 5 + dd - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146097 + doe - 719468;
    Ok(days * 86400 + hh * 3600 + mm * 60 + ss - offset_secs)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Leaf → intermediate → root bundle (ECDSA P-256, valid 2020-01-01 to
    /// 2045-01-01). Generated once with rcgen and embedded so the suite
    /// neither shells out to openssl nor depends on rcgen at runtime.
    const CHAIN: &str = include_str!("testdata/chain.pem");
    const LEAF: &str = include_str!("testdata/chain-leaf.pem");
    const INTER: &str = include_str!("testdata/chain-inter.pem");
    const ROOT: &str = include_str!("testdata/chain-root.pem");
    /// openssl-generated bundles (ECDSA P-256; leaves valid 2026-10-25 to
    /// 2027-10-25, CAs to 2036) that `openssl verify` rejects at the root:
    /// a `pathlen:0` root above an intermediate ("path length constraint
    /// exceeded"), a self-signed `CA:FALSE` root, and a root with no
    /// basicConstraints ("invalid CA certificate" for both).
    const PATHLEN0: &str = include_str!("testdata/chain-pathlen0.pem");
    const ROOT_CA_FALSE: &str = include_str!("testdata/chain-root-ca-false.pem");
    const ROOT_NO_BC: &str = include_str!("testdata/chain-root-no-bc.pem");
    /// Cross-sign regression (the AddTrust/USERTrust pattern):
    /// `chain-cross-served.pem` is a leaf plus a copy of the CA that shares
    /// the anchor's name and key but is issued by an absent older root;
    /// `chain-cross-ca.pem` is the self-signed anchor `openssl verify`
    /// accepts. Regenerate with openssl: self-signed Old Root and Cross CA
    /// (`CA:true`), a Cross CA CSR signed by Old Root for the cross copy,
    /// and a leaf CSR signed by Cross CA; served = leaf + cross copy.
    /// Root/CA fixtures below: `chain-ku-root.pem` is a leaf plus a
    /// self-signed root with keyUsage keyCertSign but no basicConstraints
    /// (openssl accepts); `chain-badku.pem` is a leaf plus an intermediate
    /// whose keyUsage omits keyCertSign (openssl rejects); `chain-norm.pem`
    /// is a leaf plus an intermediate whose subject is the same name in a
    /// different string type and case (openssl accepts after normalizing).
    /// `chain-v1-root.pem` is a v1 root derived from an openssl v3 root by
    /// flipping the version INTEGER 2→0 in the DER (same bytes otherwise;
    /// self-signature no longer verifies, so it only serves as a --ca
    /// anchor, which openssl accepts); `chain-v1-leaf.pem` is its leaf.
    /// `chain-p521-*` (ECDSA P-521) and `chain-pss-*` (RSA-PSS) are valid
    /// chains openssl accepts but ring cannot verify: tool errors, not
    /// verdicts. `chain-rsa1024-*` is a 1024-bit RSA chain in the same boat.
    /// `chain-rootku-*` is a leaf plus a root with CA:TRUE but keyUsage
    /// without keyCertSign (openssl rejects anchors like this too).
    /// All generated with openssl 3.5; validity starts at
    /// generation, so tests pin `now` to each fixture's own window.
    const KU_ROOT: &str = include_str!("testdata/chain-ku-root.pem");
    const BADKU: &str = include_str!("testdata/chain-badku.pem");
    const NORM: &str = include_str!("testdata/chain-norm.pem");
    const V1_LEAF: &str = include_str!("testdata/chain-v1-leaf.pem");
    const V1_ROOT: &str = include_str!("testdata/chain-v1-root.pem");
    const P521_LEAF: &str = include_str!("testdata/chain-p521-leaf.pem");
    const P521_ROOT: &str = include_str!("testdata/chain-p521-root.pem");
    /// 1024-bit RSA chain (openssl accepts at the default security level;
    /// ring floors RSA at 2048 bits for SHA-256): "can't check", not invalid.
    const SMALL_LEAF: &str = include_str!("testdata/chain-rsa1024-leaf.pem");
    const SMALL_ROOT: &str = include_str!("testdata/chain-rsa1024-root.pem");
    /// Same 1024-bit key with SHA-1 signatures: ring's floor is 1024 bits
    /// for SHA-1, so this chain verifies cleanly.
    const SMALL_SHA1: &str = include_str!("testdata/chain-rsa1024-sha1.pem");
    /// 2041-bit RSA chain: ring rounds the modulus up to whole bytes (256),
    /// so it evaluates the signatures. Exact-bit accounting would wrongly
    /// report "can't check" here.
    const EDGE_LEAF: &str = include_str!("testdata/chain-rsa2041-leaf.pem");
    const EDGE_ROOT: &str = include_str!("testdata/chain-rsa2041-root.pem");
    /// Root with CA:TRUE but keyUsage lacking keyCertSign (openssl rejects
    /// anchors like this too): single definite issue, no exemption.
    const ROOTKU_LEAF: &str = include_str!("testdata/chain-rootku-leaf.pem");
    const ROOTKU_ROOT: &str = include_str!("testdata/chain-rootku-root.pem");
    const PSS_LEAF: &str = include_str!("testdata/chain-pss-leaf.pem");
    const PSS_ROOT: &str = include_str!("testdata/chain-pss-root.pem");
    const CROSS_CA: &str = include_str!("testdata/chain-cross-ca.pem");
    const CROSS_SERVED: &str = include_str!("testdata/chain-cross-served.pem");

    fn now_2027() -> i64 {
        parse_rfc3339("2027-01-01T00:00:00Z").unwrap()
    }

    /// One named pool file (`--ca` / `--untrusted`) for anchored tests.
    fn pool(pem: &str, name: &str) -> Vec<(String, Vec<Vec<u8>>)> {
        vec![(name.to_string(), ders(pem))]
    }

    fn has_issue(r: &ChainReport, needle: &str) -> bool {
        r.issues.iter().any(|i| i.contains(needle))
    }

    fn ders(pem: &str) -> Vec<Vec<u8>> {
        extract_ders(pem.as_bytes()).unwrap()
    }

    fn now_2030() -> i64 {
        parse_rfc3339("2030-06-15T12:00:00Z").unwrap()
    }

    #[test]
    fn valid_chain_passes() {
        let r = verify_one(1, "chain.pem", &ders(CHAIN), now_2030(), false).unwrap();
        assert!(r.valid, "issues: {:?}", r.issues);
        assert_eq!(r.length, 3);
        assert_eq!(r.certs[0].role, "leaf");
        assert_eq!(r.certs[1].role, "inter");
        assert_eq!(r.certs[2].role, "root");
    }

    #[test]
    fn shuffled_file_order_still_valid() {
        // Assembly follows issuer→subject links, not file order.
        let bundle = format!("{ROOT}{LEAF}{INTER}");
        let r = verify_one(1, "shuffled.pem", &ders(&bundle), now_2030(), false).unwrap();
        assert!(r.valid, "issues: {:?}", r.issues);
        assert_eq!(r.certs[0].role, "leaf");
    }

    #[test]
    fn missing_intermediate_fails_linkage() {
        let bundle = format!("{LEAF}{ROOT}");
        let r = verify_one(1, "gap.pem", &ders(&bundle), now_2030(), false).unwrap();
        assert!(!r.valid);
        assert!(
            r.issues.iter().any(|i| i.contains("does not match")),
            "issues: {:?}",
            r.issues
        );
    }

    #[test]
    fn tampered_signature_fails() {
        // Flip the last DER byte (inside the signature BIT STRING): the
        // cert still parses, but the signature no longer verifies.
        let mut ders = ders(CHAIN);
        let last = ders[0].len() - 1;
        ders[0][last] ^= 0xff;
        let r = verify_one(1, "tampered.pem", &ders, now_2030(), false).unwrap();
        assert!(!r.valid);
        assert!(
            r.issues.iter().any(|i| i.contains("signature")),
            "issues: {:?}",
            r.issues
        );
    }

    #[test]
    fn incomplete_chain_missing_root_fails() {
        let bundle = format!("{LEAF}{INTER}");
        let r = verify_one(1, "noroot.pem", &ders(&bundle), now_2030(), false).unwrap();
        assert!(!r.valid);
        assert!(
            r.issues.iter().any(|i| i.contains("non-self-signed")),
            "issues: {:?}",
            r.issues
        );
    }

    #[test]
    fn at_before_not_before_fails() {
        let at = parse_rfc3339("2019-06-15T12:00:00Z").unwrap();
        let r = verify_one(1, "chain.pem", &ders(CHAIN), at, false).unwrap();
        assert!(!r.valid);
        assert!(
            r.issues.iter().any(|i| i.contains("not yet valid")),
            "issues: {:?}",
            r.issues
        );
    }

    #[test]
    fn at_after_not_after_fails() {
        let at = parse_rfc3339("2046-01-01T00:00:00Z").unwrap();
        let r = verify_one(1, "chain.pem", &ders(CHAIN), at, false).unwrap();
        assert!(!r.valid);
        assert!(
            r.issues.iter().any(|i| i.contains("expired")),
            "issues: {:?}",
            r.issues
        );
    }

    #[test]
    fn single_self_signed_needs_flag() {
        let ders = ders(crate::cert::tests::TEST_PEM);
        let denied = verify_one(1, "cert.pem", &ders, now_2030(), false).unwrap();
        assert!(!denied.valid);
        assert!(
            denied
                .issues
                .iter()
                .any(|i| i.contains("--allow-self-signed")),
            "issues: {:?}",
            denied.issues
        );
        let allowed = verify_one(1, "cert.pem", &ders, now_2030(), true).unwrap();
        assert!(allowed.valid, "issues: {:?}", allowed.issues);
        assert_eq!(allowed.certs[0].role, "root");
    }

    #[test]
    fn rfc3339_zulu_and_offsets_agree() {
        let z = parse_rfc3339("2030-01-01T00:00:00Z").unwrap();
        let plus = parse_rfc3339("2030-01-01T02:00:00+02:00").unwrap();
        let minus = parse_rfc3339("2029-12-31T19:00:00-05:00").unwrap();
        assert_eq!(z, plus);
        assert_eq!(z, minus);
        // Epoch sanity: 1970-01-01T00:00:00Z is zero.
        assert_eq!(parse_rfc3339("1970-01-01T00:00:00Z").unwrap(), 0);
    }

    #[test]
    fn rfc3339_rejects_garbage() {
        for bad in [
            "2030-01-01",
            "2030-01-01 00:00:00",
            "2030-13-01T00:00:00Z",
            "2030-01-01T25:00:00Z",
            "2030-01-01T00:00:00",
            "not a date",
        ] {
            assert!(parse_rfc3339(bad).is_err(), "{bad} should fail");
        }
    }

    #[test]
    fn rfc3339_rejects_impossible_days() {
        // Strict calendar: these must fail, not roll into next month.
        // Signed fields and out-of-range offsets fail too: every field
        // must be plain ASCII digits.
        for bad in [
            "2030-04-31T00:00:00Z",
            "2030-06-31T00:00:00Z",
            "2030-02-29T00:00:00Z",
            "1900-02-29T00:00:00Z",
            "2030-02-30T00:00:00Z",
            "2030-01-01T-1:00:00Z",
            "2030-+1-01T00:00:00Z",
            "2030-01-0-1T00:00:00Z",
            "+030-01-01T00:00:00Z",
            "2030-01-01T00:00:00+99:99",
            "2030-01-01T00:00:00-05:99",
            "2030-01-01T00:00:00+-05:00",
        ] {
            assert!(parse_rfc3339(bad).is_err(), "{bad} should fail");
        }
        assert!(parse_rfc3339("2030-04-30T00:00:00Z").is_ok());
        assert!(parse_rfc3339("2028-02-29T00:00:00Z").is_ok());
        assert!(parse_rfc3339("2000-02-29T00:00:00Z").is_ok());
    }

    // ── root constraints (regression: the root was exempt) ────────────

    #[test]
    fn root_path_length_violation_fails() {
        let r = verify_one(1, "pathlen0.pem", &ders(PATHLEN0), now_2027(), false).unwrap();
        assert!(!r.valid);
        assert!(
            has_issue(&r, "path length constraint violated"),
            "{:?}",
            r.issues
        );
    }

    #[test]
    fn root_with_ca_false_fails() {
        let r = verify_one(1, "cafalse.pem", &ders(ROOT_CA_FALSE), now_2027(), false).unwrap();
        assert!(!r.valid);
        assert!(has_issue(&r, "CA:FALSE"), "{:?}", r.issues);
    }

    #[test]
    fn root_without_basic_constraints_fails() {
        let r = verify_one(1, "nobc.pem", &ders(ROOT_NO_BC), now_2027(), false).unwrap();
        assert!(!r.valid);
        assert!(has_issue(&r, "no basicConstraints"), "{:?}", r.issues);
    }

    #[test]
    fn root_constraint_failures_are_the_only_issues() {
        // Inside every validity window, with good signatures: the root
        // constraint is the one thing wrong, so it must be what's reported.
        for (name, pem) in [
            ("pathlen0", PATHLEN0),
            ("cafalse", ROOT_CA_FALSE),
            ("nobc", ROOT_NO_BC),
        ] {
            let r = verify_one(1, name, &ders(pem), now_2027(), false).unwrap();
            assert_eq!(r.issues.len(), 1, "{name}: {:?}", r.issues);
        }
    }

    // ── anchored mode (--ca / --untrusted) ────────────────────────────

    #[test]
    fn anchored_matches_openssl_verify_layout() {
        // `--ca root.pem --untrusted inter.pem leaf.pem`, the shape of
        // `openssl verify -CAfile root.pem -untrusted inter.pem leaf.pem`.
        let r = verify_anchored(
            1,
            "leaf.pem",
            &ders(LEAF),
            &pool(INTER, "inter.pem"),
            &pool(ROOT, "root.pem"),
            now_2030(),
        )
        .unwrap();
        assert!(r.valid, "issues: {:?}", r.issues);
        let roles: Vec<&str> = r.certs.iter().map(|c| c.role.as_str()).collect();
        assert_eq!(roles, ["leaf", "inter", "root"]);
    }

    #[test]
    fn anchored_uses_intermediates_carried_by_the_input() {
        // A server bundle (leaf + intermediate) against a CA bundle that
        // also holds an unrelated root.
        let target = format!("{LEAF}{INTER}");
        let ca = format!("{}{ROOT}", crate::cert::tests::TEST_PEM);
        let r = verify_anchored(
            1,
            "server.pem",
            &ders(&target),
            &[],
            &pool(&ca, "ca.pem"),
            now_2030(),
        )
        .unwrap();
        assert!(r.valid, "issues: {:?}", r.issues);
        assert_eq!(r.length, 3);
    }

    #[test]
    fn anchored_without_the_intermediate_fails() {
        let r = verify_anchored(
            1,
            "leaf.pem",
            &ders(LEAF),
            &[],
            &pool(ROOT, "root.pem"),
            now_2030(),
        )
        .unwrap();
        assert!(!r.valid);
        assert!(has_issue(&r, "issuer not present"), "{:?}", r.issues);
    }

    #[test]
    fn anchored_root_must_be_a_ca_cert() {
        // The input carries its own (valid, self-signed) root, but the
        // trust anchor is something else: not trusted.
        let r = verify_anchored(
            1,
            "chain.pem",
            &ders(CHAIN),
            &[],
            &pool(crate::cert::tests::TEST_PEM, "other.pem"),
            now_2030(),
        )
        .unwrap();
        assert!(!r.valid);
        assert!(
            has_issue(&r, "is not one of the --ca certificates"),
            "{:?}",
            r.issues
        );
    }

    #[test]
    fn anchored_anchor_verifies_itself() {
        let r = verify_anchored(
            1,
            "root.pem",
            &ders(ROOT),
            &[],
            &pool(ROOT, "root.pem"),
            now_2030(),
        )
        .unwrap();
        assert!(r.valid, "issues: {:?}", r.issues);
    }

    /// Validity pinned to the fixture's own window, so openssl-generated
    /// fixture tests depend on neither wall-clock time nor hardcoded dates.
    fn valid_midpoint(ders: &[Vec<u8>]) -> i64 {
        let (_, leaf) = X509Certificate::from_der(&ders[0]).unwrap();
        (leaf.validity().not_before.timestamp() + leaf.validity().not_after.timestamp()) / 2
    }

    #[test]
    fn anchored_prefers_anchor_over_cross_signed_copy() {
        // The served bundle carries a cross-signed copy of the CA: same
        // name and key as the anchor, but issued by the absent Old Root.
        // `openssl verify -CAfile chain-cross-ca.pem chain-cross-served.pem`
        // is OK, so the anchor must win the issuer pick.
        let served = ders(CROSS_SERVED);
        let anchor_pool = pool(CROSS_CA, "ca.pem");
        let (_, copy) = X509Certificate::from_der(&served[1]).unwrap();
        let (_, anchor) = X509Certificate::from_der(&anchor_pool[0].1[0]).unwrap();
        assert_ne!(copy.subject(), copy.issuer());
        assert_eq!(copy.subject(), anchor.subject());
        let now = valid_midpoint(&served);
        let r = verify_anchored(1, "served.pem", &served, &[], &anchor_pool, now).unwrap();
        assert!(r.valid, "issues: {:?}", r.issues);
        let roles: Vec<&str> = r.certs.iter().map(|c| c.role.as_str()).collect();
        assert_eq!(roles, ["leaf", "root"]);
        // The openssl layout (leaf verified, bundle as --untrusted) agrees.
        let untrusted = vec![("served.pem".to_string(), served[1..].to_vec())];
        let r2 =
            verify_anchored(1, "leaf.pem", &served[..1], &untrusted, &anchor_pool, now).unwrap();
        assert!(r2.valid, "issues: {:?}", r2.issues);
    }

    #[test]
    fn cross_signed_bundle_without_anchor_stays_invalid() {
        // Bundle mode has no trust anchor, so the cross-signed copy's
        // absent issuer genuinely ends the chain: invalid, and the last
        // cert is an inter (not a root) by role.
        let served = ders(CROSS_SERVED);
        let r = verify_one(1, "served.pem", &served, valid_midpoint(&served), false).unwrap();
        assert!(!r.valid);
        assert_eq!(r.certs[1].role, "inter");
        assert!(has_issue(&r, "issuer not present"), "{:?}", r.issues);
    }

    // ── normalized names, legacy roots, keyUsage ─────────────────────

    #[test]
    fn names_ignore_string_type_case_and_spaces() {
        // The bundle's intermediate is the same name as the leaf's issuer
        // in another string type and case (PrintableString "NORM INTER"
        // vs UTF8String "Norm Inter"). Byte comparison would break the
        // linkage; openssl normalizes and accepts.
        let bundle = ders(NORM);
        let (_, leaf) = X509Certificate::from_der(&bundle[0]).unwrap();
        let (_, inter) = X509Certificate::from_der(&bundle[1]).unwrap();
        assert_ne!(leaf.issuer(), inter.subject());
        assert!(names_equal(leaf.issuer(), inter.subject()));
        let r = verify_one(1, "norm.pem", &bundle, valid_midpoint(&bundle), false).unwrap();
        assert!(r.valid, "issues: {:?}", r.issues);
        assert_eq!(r.length, 3);
    }

    #[test]
    fn v1_root_accepted() {
        // A self-signed X.509 v1 root is accepted as an anchor, like
        // openssl. (Derived from an openssl v3 root by flipping the
        // version INTEGER 2→0; extensions retained, self-signature no
        // longer verifies, so it only serves as a --ca anchor, for which
        // self-signature checks are skipped.)
        let leaf = ders(V1_LEAF);
        let anchors = pool(V1_ROOT, "root.pem");
        let now = valid_midpoint(&leaf);
        let r = verify_anchored(1, "leaf.pem", &leaf, &[], &anchors, now).unwrap();
        assert!(r.valid, "issues: {:?}", r.issues);
        assert_eq!(r.length, 2);
    }

    #[test]
    fn root_with_keycertsign_but_no_bc_accepted() {
        // A self-signed root with keyUsage keyCertSign but no
        // basicConstraints is accepted, like openssl in non-strict mode.
        let bundle = ders(KU_ROOT);
        let r = verify_one(1, "ku.pem", &bundle, valid_midpoint(&bundle), false).unwrap();
        assert!(r.valid, "issues: {:?}", r.issues);
        assert_eq!(r.length, 2);
    }

    #[test]
    fn intermediate_keyusage_without_keycertsign_fails() {
        // An intermediate whose keyUsage omits keyCertSign is invalid,
        // like openssl ("key usage does not include certificate signing").
        let bundle = ders(BADKU);
        let r = verify_one(1, "badku.pem", &bundle, valid_midpoint(&bundle), false).unwrap();
        assert!(!r.valid);
        assert_eq!(r.issues.len(), 1, "{:?}", r.issues);
        assert!(
            has_issue(&r, "keyUsage does not include keyCertSign"),
            "{:?}",
            r.issues
        );
    }

    // ── unevaluatable signatures are "can't check", not invalid ──────

    #[test]
    fn unsupported_signature_algorithm_is_unverified() {
        // P-521 and RSA-PSS chains are valid and openssl accepts them, but
        // ring cannot verify them: no definite issue, one uncertain note,
        // status unverified (not valid).
        for (name, pem) in [("p521", P521_LEAF), ("pss", PSS_LEAF)] {
            let leaf = ders(pem);
            let anchors = pool(
                if name == "p521" { P521_ROOT } else { PSS_ROOT },
                "root.pem",
            );
            let r = verify_anchored(1, "leaf.pem", &leaf, &[], &anchors, valid_midpoint(&leaf))
                .unwrap();
            assert!(!r.valid, "{name}: {r:?}");
            assert!(r.issues.is_empty(), "{name}: {:?}", r.issues);
            assert_eq!(chain_status(&r), "unverified", "{name}: {r:?}");
            assert_eq!(r.uncertain.len(), 1, "{name}: {:?}", r.uncertain);
            assert!(
                r.uncertain[0].contains("can't check cert 1"),
                "{name}: {:?}",
                r.uncertain
            );
        }
    }

    #[test]
    fn undersized_rsa_key_is_unverified() {
        // A 1024-bit RSA chain is valid and openssl accepts it at the
        // default security level, but ring only handles 2048–8192-bit RSA
        // keys (1024–8192 for SHA-1): no definite issue, one uncertain
        // note, status unverified (not valid).
        let leaf = ders(SMALL_LEAF);
        let anchors = pool(SMALL_ROOT, "root.pem");
        let r =
            verify_anchored(1, "leaf.pem", &leaf, &[], &anchors, valid_midpoint(&leaf)).unwrap();
        assert!(!r.valid, "{:?}", r);
        assert!(r.issues.is_empty(), "{:?}", r.issues);
        assert_eq!(chain_status(&r), "unverified", "{:?}", r);
        assert_eq!(r.uncertain.len(), 1, "{:?}", r.uncertain);
        assert!(r.uncertain[0].contains("1024-bit"), "{:?}", r.uncertain);
    }

    #[test]
    fn sha1_rsa_1024_verifies() {
        // Ring's RSA floor drops to 1024 bits for SHA-1 signatures: the
        // pairwise SHA-1 check verifies and the --ca anchor skips its own
        // check, so this chain is valid with no uncertain notes (contrast
        // the SHA-256 1024-bit chain above).
        let all = ders(SMALL_SHA1);
        let anchors = vec![("root.pem".to_string(), all[1..].to_vec())];
        let r = verify_anchored(
            1,
            "leaf.pem",
            &all[..1],
            &[],
            &anchors,
            valid_midpoint(&all),
        )
        .unwrap();
        assert!(r.valid, "{:?}", r.issues);
        assert!(r.issues.is_empty(), "{:?}", r.issues);
        assert!(r.uncertain.is_empty(), "{:?}", r.uncertain);
    }

    #[test]
    fn chain_status_words() {
        // The status word behind the chain row: valid needs both lists
        // empty, any definite issue wins over uncertain notes.
        let clean = report(1, "s", vec![], vec![], vec![]);
        assert_eq!(chain_status(&clean), "valid");
        assert!(clean.valid);
        let bad = report(1, "s", vec![], vec!["i".into()], vec!["u".into()]);
        assert_eq!(chain_status(&bad), "invalid");
        assert!(!bad.valid);
        let un = report(1, "s", vec![], vec![], vec!["u".into()]);
        assert_eq!(chain_status(&un), "unverified");
        assert!(!un.valid);
    }

    #[test]
    fn sub_2048_bit_key_at_whole_bytes_verifies() {
        // The 2041-bit RSA chain occupies whole 256-byte moduli, so ring
        // evaluates the signatures: valid, no uncertain notes. Exact-bit
        // accounting would wrongly report "can't check" here.
        let leaf = ders(EDGE_LEAF);
        let anchors = pool(EDGE_ROOT, "root.pem");
        let r =
            verify_anchored(1, "leaf.pem", &leaf, &[], &anchors, valid_midpoint(&leaf)).unwrap();
        assert!(r.valid, "{:?}", r);
        assert!(r.issues.is_empty(), "{:?}", r.issues);
        assert!(r.uncertain.is_empty(), "{:?}", r.uncertain);
    }

    #[test]
    fn bad_signature_at_whole_bytes_is_invalid_not_uncertain() {
        // A genuinely bad signature from a 2041-bit key is a verdict
        // (invalid), not a "can't check": ring evaluates these keys.
        let mut leaf = ders(EDGE_LEAF);
        let last = leaf[0].len() - 1;
        leaf[0][last] ^= 0xff;
        let anchors = pool(EDGE_ROOT, "root.pem");
        let r =
            verify_anchored(1, "leaf.pem", &leaf, &[], &anchors, valid_midpoint(&leaf)).unwrap();
        assert!(!r.valid);
        assert!(has_issue(&r, "does not verify"), "{:?}", r);
        assert!(r.uncertain.is_empty(), "{:?}", r.uncertain);
    }

    #[test]
    fn definite_problem_beats_uncertain_note() {
        // A chain carrying its own PSS root that isn't in --ca: the
        // pairwise PSS signature can't be checked, but the definite
        // "not one of the --ca certificates" finding decides invalid.
        let input = format!("{PSS_LEAF}{PSS_ROOT}");
        let anchors = pool(crate::cert::tests::TEST_PEM, "other.pem");
        let all = ders(&input);
        let r = verify_anchored(1, "chain.pem", &all, &[], &anchors, valid_midpoint(&all)).unwrap();
        assert!(!r.valid);
        assert!(has_issue(&r, "not one of the --ca certificates"), "{:?}", r);
        // Both signatures are PSS: the pairwise check and the non-anchor
        // root's self-signature genuinely can't be evaluated.
        assert_eq!(r.uncertain.len(), 2, "{:?}", r.uncertain);
        assert!(
            r.uncertain
                .iter()
                .all(|u| u.contains("algorithm not supported")),
            "{:?}",
            r.uncertain
        );
    }

    #[test]
    fn uncertain_only_chain_errors_at_run() {
        // run() maps valid-but-uncertain to a tool error (exit 2).
        let dir = tempfile::tempdir().unwrap();
        let write = |name: &str, pem: &str| {
            let p = dir.path().join(name);
            std::fs::write(&p, pem).unwrap();
            p
        };
        let leaf = write("leaf.pem", P521_LEAF);
        let root = write("root.pem", P521_ROOT);
        let args = VerifyChainArgs {
            files: vec![leaf],
            ca: vec![root],
            untrusted: vec![],
            format: ChainFormat::Kv,
            json: false,
            at: None,
            allow_self_signed: false,
            limit: None,
        };
        let err = run(&args).unwrap_err();
        assert!(err.to_string().contains("can't check"), "{err:?}");
    }

    #[test]
    fn root_keyusage_without_keycertsign_fails() {
        // A root with CA:TRUE but keyUsage lacking keyCertSign is invalid;
        // trust anchors get no exemption (openssl rejects them too).
        let bundle = ders(&format!("{ROOTKU_LEAF}{ROOTKU_ROOT}"));
        let r = verify_one(1, "rootku.pem", &bundle, valid_midpoint(&bundle), false).unwrap();
        assert!(!r.valid);
        assert_eq!(r.issues.len(), 1, "{:?}", r.issues);
        assert!(
            has_issue(&r, "keyUsage does not include keyCertSign"),
            "{:?}",
            r.issues
        );
        assert!(r.uncertain.is_empty(), "{:?}", r.uncertain);
    }

    #[test]
    fn trusted_anchor_skips_self_signature_check() {
        // A byte-matched --ca anchor is trusted as-is: its PSS
        // self-signature (which sak cannot verify) is not checked.
        let root = ders(PSS_ROOT);
        let anchors = pool(PSS_ROOT, "root.pem");
        let r =
            verify_anchored(1, "root.pem", &root, &[], &anchors, valid_midpoint(&root)).unwrap();
        assert!(r.valid, "issues: {:?}", r.issues);
    }

    #[test]
    fn bad_anchor_file_names_its_own_file() {
        // A bad cert in ca.pem is reported at ca.pem, not at the leaf's file.
        let anchors = vec![("ca.pem".to_string(), vec![b"hello world".to_vec()])];
        let err =
            verify_anchored(1, "leaf.pem", &ders(LEAF), &[], &anchors, now_2030()).unwrap_err();
        assert!(
            err.to_string().contains("parsing certificate at ca.pem#0"),
            "{err:?}",
        );
    }

    #[test]
    fn anchored_still_checks_root_constraints() {
        // openssl verify -CAfile <pathlen:0 root> rejects this chain too.
        let all = ders(PATHLEN0);
        let anchors = vec![("root.pem".to_string(), all[2..].to_vec())];
        let r = verify_anchored(1, "leaf.pem", &all[..2], &[], &anchors, now_2027()).unwrap();
        assert!(!r.valid);
        assert!(
            has_issue(&r, "path length constraint violated"),
            "{:?}",
            r.issues
        );
    }

    // ── flags and run ─────────────────────────────────────────────────

    /// Parse argv through clap so `requires` / `conflicts_with` apply.
    fn parse_args(argv: &[&str]) -> Result<VerifyChainArgs, clap::Error> {
        use clap::Parser;
        #[derive(Parser)]
        struct Cli {
            #[command(flatten)]
            args: VerifyChainArgs,
        }
        Cli::try_parse_from(std::iter::once("verify-chain").chain(argv.iter().copied()))
            .map(|cli| cli.args)
    }

    #[test]
    fn untrusted_needs_ca_and_self_signed_flag_conflicts_with_it() {
        assert!(parse_args(&["--untrusted", "i.pem", "leaf.pem"]).is_err());
        assert!(parse_args(&["--ca", "r.pem", "--allow-self-signed", "leaf.pem"]).is_err());
        let ok = parse_args(&[
            "--ca",
            "r.pem",
            "--ca",
            "r2.pem",
            "--untrusted",
            "i.pem",
            "leaf.pem",
        ])
        .unwrap();
        assert_eq!(ok.ca.len(), 2);
        assert_eq!(ok.untrusted.len(), 1);
    }

    #[test]
    fn run_exit_codes_follow_validity() {
        let dir = tempfile::tempdir().unwrap();
        let write = |name: &str, pem: &str| {
            let p = dir.path().join(name);
            std::fs::write(&p, pem).unwrap();
            p
        };
        let (leaf, inter, root) = (
            write("leaf.pem", LEAF),
            write("inter.pem", INTER),
            write("root.pem", ROOT),
        );
        let at = Some("2030-06-15T12:00:00Z".to_string());
        let base = || VerifyChainArgs {
            files: vec![leaf.clone()],
            ca: vec![root.clone()],
            untrusted: vec![inter.clone()],
            format: ChainFormat::Kv,
            json: false,
            at: at.clone(),
            allow_self_signed: false,
            limit: None,
        };
        assert_eq!(run(&base()).unwrap(), Outcome::Found);
        // Same files, no intermediate: invalid is exit 1, not an error.
        let missing = VerifyChainArgs {
            untrusted: vec![],
            ..base()
        };
        assert_eq!(run(&missing).unwrap(), Outcome::NotFound);
        // Bundle mode treats each file as its own chain, so the openssl
        // layout without --ca is three incomplete chains.
        let bundle = VerifyChainArgs {
            files: vec![leaf.clone(), inter.clone(), root.clone()],
            ca: vec![],
            untrusted: vec![],
            ..base()
        };
        assert_eq!(run(&bundle).unwrap(), Outcome::NotFound);
    }
}
