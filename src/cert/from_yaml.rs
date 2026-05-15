use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{Context, Result, bail};
use clap::Args;
use serde_json::Value;

use crate::cert::{
    CertInfo, FIELD_NAMES, OutputFormat, extract_ders, from_kubeconfig, inspect, parse_cert,
};
use crate::value::{parse_dot_path, resolve_path};

#[derive(Args)]
#[command(
    about = "Inspect a certificate extracted from a YAML document at a path",
    long_about = "Extract a string from a path inside a YAML document and \
        inspect it as one or more X.509 certificates.\n\nThe extracted value \
        may be a raw PEM block, a base64-wrapped PEM (the shape Kubernetes \
        Secrets and Talos resources use), or even an entire embedded \
        kubeconfig — `from-yaml` auto-detects which and walks accordingly.\n\n\
        Path syntax matches `sak json query` / `sak config query`: dot \
        notation (`.spec.adminKubeconfig`) or JSON Pointer \
        (`/spec/adminKubeconfig`). Use JSON Pointer for keys that themselves \
        contain dots (e.g. `/data/tls.crt` for a Kubernetes Secret).\n\n\
        Composes with `kubectl get … -o yaml` and `talosctl get … -o yaml` \
        via stdin (use `--path` and pipe the YAML in).",
    after_help = "\
Examples:
  sak cert from-yaml secret.yaml --path .data.tls.crt
  sak cert from-yaml talos-secrets.yaml --path .spec.adminKubeconfig
  kubectl get secret tls -o yaml | sak cert from-yaml --path .data.tls.crt -
  sak cert from-yaml --path .data.ca.crt --json secret.yaml"
)]
pub struct FromYamlArgs {
    /// YAML file (use `-` for stdin)
    pub file: PathBuf,

    /// Path to the embedded cert/kubeconfig string (dot notation or JSON Pointer)
    #[arg(long)]
    pub path: String,

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

pub fn run(args: &FromYamlArgs) -> Result<ExitCode> {
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

    let bytes = if args.file.as_os_str() == "-" {
        let mut buf = Vec::new();
        std::io::Read::read_to_end(&mut std::io::stdin(), &mut buf)?;
        buf
    } else {
        std::fs::read(&args.file)
            .with_context(|| format!("cannot read: {}", args.file.display()))?
    };
    let document: Value = serde_yaml::from_slice(&bytes)
        .with_context(|| format!("invalid YAML: {}", args.file.display()))?;

    let extracted = extract_string_at_path(&document, &args.path)?;
    let source = format!("{}#{}", args.file.display(), args.path);

    let infos = certs_from_string(&source, &extracted)?;

    if infos.is_empty() {
        return Ok(ExitCode::from(1));
    }

    inspect::emit(&infos, format, args.field.as_deref(), args.limit)
}

/// Resolve `path` against `document` and return the string value, or an
/// error if the path is missing or doesn't point at a string.
fn extract_string_at_path(document: &Value, path: &str) -> Result<String> {
    let resolved = if path.starts_with('/') || path.is_empty() {
        document.pointer(path)
    } else {
        let segments = parse_dot_path(path)?;
        resolve_path(document, &segments)
    };
    let value = resolved.with_context(|| format!("path `{}` not found in document", path))?;
    match value {
        Value::String(s) => Ok(s.clone()),
        other => bail!(
            "path `{}` resolved to a {} — expected a string",
            path,
            type_name(other)
        ),
    }
}

fn type_name(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "bool",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

/// Auto-detect: is `s` a raw cert / PEM bundle / base64-PEM, or a kubeconfig?
/// Try cert extraction first (cheap, succeeds for the common case); if that
/// fails, try parsing as a YAML kubeconfig and walking its users/clusters.
pub fn certs_from_string(source: &str, s: &str) -> Result<Vec<CertInfo>> {
    if let Ok(ders) = extract_ders(s.as_bytes()) {
        let mut out = Vec::with_capacity(ders.len());
        for (i, der) in ders.iter().enumerate() {
            out.push(parse_cert(source, i, der, "")?);
        }
        return Ok(out);
    }

    // Fall back: maybe the extracted blob is itself a kubeconfig.
    if let Ok(kc) = serde_yaml::from_str::<Value>(s) {
        let infos =
            from_kubeconfig::extract_kubeconfig_certs(source, &kc, None, true).unwrap_or_default();
        if !infos.is_empty() {
            return Ok(infos);
        }
    }

    bail!("extracted value is neither a certificate nor a kubeconfig with embedded certs")
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine;
    use std::io::Write;

    fn write_tmp_yaml(content: &str) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("doc.yaml");
        let mut f = std::fs::File::create(&p).unwrap();
        f.write_all(content.as_bytes()).unwrap();
        (dir, p)
    }

    #[test]
    fn from_yaml_extracts_base64_pem_dot_path() {
        let b64 = base64::engine::general_purpose::STANDARD.encode(crate::cert::tests::TEST_PEM);
        let yaml = format!("data:\n  cert: {}\n", b64);
        let (_d, p) = write_tmp_yaml(&yaml);
        let args = FromYamlArgs {
            file: p,
            path: ".data.cert".to_string(),
            format: OutputFormat::Kv,
            json: false,
            tsv: false,
            field: None,
            limit: None,
        };
        assert_eq!(run(&args).unwrap(), ExitCode::SUCCESS);
    }

    #[test]
    fn from_yaml_extracts_base64_pem_pointer_with_dotted_key() {
        // Real-world k8s Secrets use dotted keys like `tls.crt` — the dot
        // path parser splits on `.`, so JSON Pointer is the right syntax for
        // that shape. This test pins the contract.
        let b64 = base64::engine::general_purpose::STANDARD.encode(crate::cert::tests::TEST_PEM);
        let yaml = format!("data:\n  tls.crt: {}\n", b64);
        let (_d, p) = write_tmp_yaml(&yaml);
        let args = FromYamlArgs {
            file: p,
            path: "/data/tls.crt".to_string(),
            format: OutputFormat::Kv,
            json: false,
            tsv: false,
            field: None,
            limit: None,
        };
        assert_eq!(run(&args).unwrap(), ExitCode::SUCCESS);
    }

    #[test]
    fn from_yaml_path_missing_errors() {
        let (_d, p) = write_tmp_yaml("data: {}\n");
        let args = FromYamlArgs {
            file: p,
            path: ".data.cert".to_string(),
            format: OutputFormat::Kv,
            json: false,
            tsv: false,
            field: None,
            limit: None,
        };
        assert!(run(&args).is_err());
    }

    #[test]
    fn from_yaml_extracts_embedded_kubeconfig() {
        let client_b64 =
            base64::engine::general_purpose::STANDARD.encode(crate::cert::tests::TEST_PEM);
        let kubeconfig_yaml = format!(
            "apiVersion: v1\nkind: Config\nusers:\n- name: alice\n  user:\n    client-certificate-data: {}\n",
            client_b64
        );
        let yaml = format!(
            "spec:\n  adminKubeconfig: |\n{}\n",
            kubeconfig_yaml
                .lines()
                .map(|l| format!("    {}", l))
                .collect::<Vec<_>>()
                .join("\n")
        );
        let (_d, p) = write_tmp_yaml(&yaml);
        let args = FromYamlArgs {
            file: p,
            path: ".spec.adminKubeconfig".to_string(),
            format: OutputFormat::Kv,
            json: false,
            tsv: false,
            field: None,
            limit: None,
        };
        assert_eq!(run(&args).unwrap(), ExitCode::SUCCESS);
    }
}
