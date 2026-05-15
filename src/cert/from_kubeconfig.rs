use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{Context, Result};
use clap::Args;
use serde_json::Value;

use crate::cert::{CertInfo, FIELD_NAMES, OutputFormat, extract_ders, inspect, parse_cert};

#[derive(Args)]
#[command(
    about = "Inspect certificates embedded in a kubeconfig file",
    long_about = "Extract and inspect the X.509 certificates embedded in a \
        kubeconfig file.\n\nWalks every entry under `users[]` and prints the \
        cert from `client-certificate-data` (base64-wrapped PEM). With --ca, \
        also walks every entry under `clusters[]` and prints \
        `certificate-authority-data`. Filter to a single name with --user. \
        The `context` field on each cert names the kubeconfig user (or \
        `cluster=<name>` for CAs) so you can tell them apart in mixed output.\n\n\
        Output flags mirror `sak cert inspect`: --json, --tsv, --field <name>.",
    after_help = "\
Examples:
  sak cert from-kubeconfig ~/.kube/config             All client certs
  sak cert from-kubeconfig ~/.kube/config --ca        Add cluster CA certs
  sak cert from-kubeconfig --user admin kubeconfig    Filter to one user
  sak cert from-kubeconfig --field not_after kc.yaml  Just expiry dates
  sak cert from-kubeconfig --json kubeconfig          JSON for tooling"
)]
pub struct FromKubeconfigArgs {
    /// Kubeconfig YAML file
    pub file: PathBuf,

    /// Filter to a specific user name (and, with --ca, cluster name)
    #[arg(long, value_name = "NAME")]
    pub user: Option<String>,

    /// Also include cluster CA certs from clusters[].cluster.certificate-authority-data
    #[arg(long)]
    pub ca: bool,

    /// Output format (default: kv)
    #[arg(long, value_enum, default_value_t = OutputFormat::Kv, conflicts_with = "field")]
    pub format: OutputFormat,

    /// Convenience for --format json
    #[arg(long, conflicts_with_all = ["tsv", "field", "format"])]
    pub json: bool,

    /// Convenience for --format tsv
    #[arg(long, conflicts_with_all = ["json", "field", "format"])]
    pub tsv: bool,

    /// Print only this field, one value per cert
    #[arg(long, value_name = "NAME")]
    pub field: Option<String>,

    /// Maximum number of output lines
    #[arg(long)]
    pub limit: Option<usize>,
}

pub fn run(args: &FromKubeconfigArgs) -> Result<ExitCode> {
    let format = if args.json {
        OutputFormat::Json
    } else if args.tsv {
        OutputFormat::Tsv
    } else {
        args.format
    };

    if let Some(field) = &args.field
        && !FIELD_NAMES.contains(&field.as_str())
    {
        anyhow::bail!(
            "unknown --field `{}` (valid: {})",
            field,
            FIELD_NAMES.join(", ")
        );
    }

    let bytes = std::fs::read(&args.file)
        .with_context(|| format!("cannot read: {}", args.file.display()))?;
    let kubeconfig: Value = serde_yaml::from_slice(&bytes)
        .with_context(|| format!("invalid YAML: {}", args.file.display()))?;

    let source = args.file.display().to_string();
    let infos = extract_kubeconfig_certs(&source, &kubeconfig, args.user.as_deref(), args.ca)?;

    if infos.is_empty() {
        return Ok(ExitCode::from(1));
    }

    inspect::emit(&infos, format, args.field.as_deref(), args.limit)
}

/// Walk a parsed kubeconfig and produce a CertInfo for every embedded cert
/// that matches the (optional) name filter. Public for reuse by `from-yaml`
/// when it auto-detects that the extracted blob is itself a kubeconfig.
pub fn extract_kubeconfig_certs(
    source: &str,
    kubeconfig: &Value,
    name_filter: Option<&str>,
    include_ca: bool,
) -> Result<Vec<CertInfo>> {
    let mut out = Vec::new();

    if let Some(users) = kubeconfig.get("users").and_then(Value::as_array) {
        for entry in users {
            let name = entry.get("name").and_then(Value::as_str).unwrap_or("");
            if let Some(filter) = name_filter
                && name != filter
            {
                continue;
            }
            let user = match entry.get("user") {
                Some(u) => u,
                None => continue,
            };
            // Two shapes: `client-certificate-data` (base64) and
            // `client-certificate` (file path). Inline data takes precedence
            // because that's the form most commonly used in service accounts
            // and cluster auth-proxy outputs.
            if let Some(data) = user.get("client-certificate-data").and_then(Value::as_str) {
                push_certs(&mut out, source, &format!("user={}", name), data.as_bytes())?;
            } else if let Some(path) = user.get("client-certificate").and_then(Value::as_str) {
                let bytes = std::fs::read(path)
                    .with_context(|| format!("cannot read referenced cert file: {}", path))?;
                push_certs(&mut out, source, &format!("user={}", name), &bytes)?;
            }
        }
    }

    if include_ca && let Some(clusters) = kubeconfig.get("clusters").and_then(Value::as_array) {
        for entry in clusters {
            let name = entry.get("name").and_then(Value::as_str).unwrap_or("");
            if let Some(filter) = name_filter
                && name != filter
            {
                continue;
            }
            let cluster = match entry.get("cluster") {
                Some(c) => c,
                None => continue,
            };
            if let Some(data) = cluster
                .get("certificate-authority-data")
                .and_then(Value::as_str)
            {
                push_certs(
                    &mut out,
                    source,
                    &format!("cluster={}", name),
                    data.as_bytes(),
                )?;
            } else if let Some(path) = cluster.get("certificate-authority").and_then(Value::as_str)
            {
                let bytes = std::fs::read(path)
                    .with_context(|| format!("cannot read referenced CA file: {}", path))?;
                push_certs(&mut out, source, &format!("cluster={}", name), &bytes)?;
            }
        }
    }

    Ok(out)
}

fn push_certs(out: &mut Vec<CertInfo>, source: &str, context: &str, bytes: &[u8]) -> Result<()> {
    let ders = extract_ders(bytes)
        .map_err(|e| anyhow::anyhow!("no certificate found at {}: {}", context, e))?;
    for (i, der) in ders.iter().enumerate() {
        out.push(parse_cert(source, i, der, context)?);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine;
    use std::io::Write;

    fn write_tmp_kubeconfig(client_pem: &str, ca_pem: &str) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("kubeconfig");
        let client_b64 = base64::engine::general_purpose::STANDARD.encode(client_pem);
        let ca_b64 = base64::engine::general_purpose::STANDARD.encode(ca_pem);
        let yaml = format!(
            "apiVersion: v1\nkind: Config\nclusters:\n- name: c1\n  cluster:\n    certificate-authority-data: {}\nusers:\n- name: alice\n  user:\n    client-certificate-data: {}\n",
            ca_b64, client_b64
        );
        let mut f = std::fs::File::create(&p).unwrap();
        f.write_all(yaml.as_bytes()).unwrap();
        (dir, p)
    }

    #[test]
    fn extract_users_default() {
        let (_d, p) =
            write_tmp_kubeconfig(crate::cert::tests::TEST_PEM, crate::cert::tests::TEST_PEM);
        let bytes = std::fs::read(&p).unwrap();
        let kc: Value = serde_yaml::from_slice(&bytes).unwrap();
        let infos = extract_kubeconfig_certs("kc", &kc, None, false).unwrap();
        assert_eq!(infos.len(), 1);
        assert_eq!(infos[0].context, "user=alice");
    }

    #[test]
    fn extract_with_ca() {
        let (_d, p) =
            write_tmp_kubeconfig(crate::cert::tests::TEST_PEM, crate::cert::tests::TEST_PEM);
        let bytes = std::fs::read(&p).unwrap();
        let kc: Value = serde_yaml::from_slice(&bytes).unwrap();
        let infos = extract_kubeconfig_certs("kc", &kc, None, true).unwrap();
        assert_eq!(infos.len(), 2);
        assert!(infos.iter().any(|i| i.context == "cluster=c1"));
    }

    #[test]
    fn extract_with_user_filter() {
        let (_d, p) =
            write_tmp_kubeconfig(crate::cert::tests::TEST_PEM, crate::cert::tests::TEST_PEM);
        let bytes = std::fs::read(&p).unwrap();
        let kc: Value = serde_yaml::from_slice(&bytes).unwrap();
        let infos = extract_kubeconfig_certs("kc", &kc, Some("alice"), false).unwrap();
        assert_eq!(infos.len(), 1);
        let infos = extract_kubeconfig_certs("kc", &kc, Some("bob"), false).unwrap();
        assert_eq!(infos.len(), 0);
    }
}
