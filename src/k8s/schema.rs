//! `sak k8s schema <kind>` — fetch the OpenAPI v3 schema for a kind from the
//! connected cluster and print it as JSON.
//!
//! This command does **not** use the dynamic resource pipeline (no
//! `Api<DynamicObject>`, no `discovery::resolve` against the cluster). It
//! talks to the apiserver only through [`crate::k8s::client::request_text`],
//! which is the chokepoint helper for raw GETs added in the foundation.
//!
//! # Two-hop fetch
//!
//! 1. `GET /openapi/v3` returns an index of the form
//!    `{ "paths": { "apis/apps/v1": { "serverRelativeURL": "/openapi/v3/apis/apps/v1?hash=..." }, ... } }`.
//!    The hash query string changes whenever the apiserver re-renders, and we
//!    must include it on the second hop.
//! 2. `GET <serverRelativeURL>` returns the OpenAPI document for that
//!    group/version. The schemas live under `components.schemas`, keyed by
//!    Java-package-style names like `io.k8s.api.apps.v1.Deployment`. We can't
//!    rely on the key alone — we match on each schema's
//!    `x-kubernetes-group-version-kind` array, which is the authoritative
//!    `(group, version, kind)` tag.
//!
//! # No `/openapi/v2` fallback
//!
//! Modern apiservers (1.19+) expose v3. We deliberately don't fall back to v2
//! — it keeps the implementation small and the failure mode honest.

use std::io;
use std::process::ExitCode;

use anyhow::{Context, Result, anyhow, bail};
use clap::Args;
use serde_json::Value;

use crate::k8s::{client, discovery};
use crate::output::BoundedWriter;

#[derive(Args)]
#[command(
    about = "Fetch the OpenAPI v3 schema for a kind",
    long_about = "Fetch the OpenAPI v3 schema for a Kubernetes kind from the connected \
        cluster and print it as pretty JSON.\n\n\
        The kind may be supplied as:\n\n  \
        - a plain name resolved via the builtin shortname table \
        (`Deployment`, `Pod`, `Service`, ...);\n  \
        - a fully qualified `group/version/Kind` (`apps/v1/Deployment`); or\n  \
        - a plain name plus explicit `--group` / `--version` to disambiguate \
        kinds that exist in multiple groups (e.g. `Ingress` in \
        `networking.k8s.io` vs the deprecated `extensions`).\n\n\
        This command requires an apiserver that exposes `/openapi/v3` (k8s \
        1.19+). Older clusters error out cleanly — there is no v2 fallback.",
    after_help = "\
Examples:
  sak k8s schema Deployment
  sak k8s schema Deployment | sak json query .properties.spec.type
  sak k8s schema Pod | sak json keys . --depth 2
  sak k8s schema apps/v1/Deployment             Fully qualified
  sak k8s schema Ingress --group networking.k8s.io --version v1"
)]
pub struct SchemaArgs {
    /// Kind to fetch (plain, or `group/version/Kind`)
    pub kind: String,

    /// Override the API group (e.g. `apps`, `networking.k8s.io`).
    /// Combine with `--version`.
    #[arg(long)]
    pub group: Option<String>,

    /// Override the API version (e.g. `v1`, `v1beta1`).
    /// Combine with `--group`.
    #[arg(long)]
    pub version: Option<String>,

    /// Maximum number of output lines
    #[arg(long)]
    pub limit: Option<usize>,
}

/// A `(group, version, kind)` triple resolved from the user's CLI input,
/// before any apiserver call.
struct Gvk {
    group: String,
    version: String,
    kind: String,
}

pub async fn run(args: &SchemaArgs) -> Result<ExitCode> {
    let gvk = resolve_gvk(args)?;

    let client = client::build_client().await?;

    // Hop 1: index of all OpenAPI v3 documents on this apiserver.
    let index_text = client::request_text(&client, "/openapi/v3").await?;
    let index: Value = serde_json::from_str(&index_text)
        .context("apiserver returned non-JSON for /openapi/v3 — does it expose OpenAPI v3?")?;

    let group_path_key = openapi_path_key(&gvk.group, &gvk.version);
    let server_relative_url = index
        .get("paths")
        .and_then(|p| p.get(&group_path_key))
        .and_then(|p| p.get("serverRelativeURL"))
        .and_then(Value::as_str)
        .ok_or_else(|| {
            anyhow!(
                "apiserver does not expose group/version {:?} via OpenAPI v3 \
                 (looked for paths.{}.serverRelativeURL in /openapi/v3)",
                format!("{}/{}", gvk.group, gvk.version),
                group_path_key
            )
        })?;

    // Hop 2: the actual document for that group/version. Must include the
    // hash query string from the index — without it the apiserver may serve
    // a stale or 404 response.
    let doc_text = client::request_text(&client, server_relative_url).await?;
    let doc: Value = serde_json::from_str(&doc_text)
        .with_context(|| format!("apiserver returned non-JSON for {server_relative_url}"))?;

    let schemas = doc
        .get("components")
        .and_then(|c| c.get("schemas"))
        .and_then(Value::as_object)
        .ok_or_else(|| {
            anyhow!(
                "OpenAPI document for {}/{} has no components.schemas",
                gvk.group,
                gvk.version
            )
        })?;

    let schema = schemas
        .iter()
        .find(|(_, s)| schema_matches_gvk(s, &gvk))
        .map(|(_, s)| s)
        .ok_or_else(|| {
            anyhow!(
                "kind {:?} not found in OpenAPI document for {}/{} \
                 (no components.schemas entry has a matching \
                 x-kubernetes-group-version-kind)",
                gvk.kind,
                gvk.group,
                gvk.version
            )
        })?;

    let pretty = serde_json::to_string_pretty(schema)?;

    let stdout = io::stdout();
    let handle = stdout.lock();
    let mut writer = BoundedWriter::new(handle, args.limit);
    for line in pretty.split('\n') {
        if !writer.write_line(line)? {
            break;
        }
    }
    writer.flush()?;
    Ok(ExitCode::SUCCESS)
}

/// Resolve the user's CLI input into a concrete `(group, version, kind)`
/// triple without contacting the cluster.
///
/// Precedence:
/// 1. Explicit `--group` and `--version` flags (must be supplied together).
/// 2. Fully qualified `group/version/Kind` form in the positional argument
///    (also accepts `version/Kind` for the core group).
/// 3. Builtin shortname lookup via [`discovery::lookup_builtin`].
fn resolve_gvk(args: &SchemaArgs) -> Result<Gvk> {
    match (args.group.as_deref(), args.version.as_deref()) {
        (Some(g), Some(v)) => {
            return Ok(Gvk {
                group: g.to_string(),
                version: v.to_string(),
                kind: args.kind.clone(),
            });
        }
        (Some(_), None) | (None, Some(_)) => {
            bail!("--group and --version must be supplied together");
        }
        (None, None) => {}
    }

    if let Some(parsed) = parse_qualified(&args.kind) {
        return Ok(parsed);
    }

    let gvk = discovery::lookup_builtin(&args.kind).ok_or_else(|| {
        anyhow!(
            "unknown kind {:?}: not in the builtin shortname table. \
             Pass --group/--version, or use the fully qualified \
             `group/version/Kind` form.",
            args.kind
        )
    })?;
    Ok(Gvk {
        group: gvk.group,
        version: gvk.version,
        kind: gvk.kind,
    })
}

/// Parse `group/version/Kind` (or `version/Kind` for the core group) out of
/// the positional argument. Returns `None` for plain kind names.
fn parse_qualified(s: &str) -> Option<Gvk> {
    let parts: Vec<&str> = s.split('/').collect();
    match parts.as_slice() {
        [v, k] if !v.is_empty() && !k.is_empty() => Some(Gvk {
            group: String::new(),
            version: (*v).to_string(),
            kind: (*k).to_string(),
        }),
        [g, v, k] if !g.is_empty() && !v.is_empty() && !k.is_empty() => Some(Gvk {
            group: (*g).to_string(),
            version: (*v).to_string(),
            kind: (*k).to_string(),
        }),
        _ => None,
    }
}

/// Build the index key the apiserver uses for a given group/version.
///
/// The OpenAPI v3 index keys core resources under `api/<version>` and
/// non-core groups under `apis/<group>/<version>`.
fn openapi_path_key(group: &str, version: &str) -> String {
    if group.is_empty() {
        format!("api/{version}")
    } else {
        format!("apis/{group}/{version}")
    }
}

/// Does this schema's `x-kubernetes-group-version-kind` annotation match the
/// triple we're looking for?
///
/// The annotation is an array of `{group, version, kind}` objects (the same
/// schema may be tagged for multiple GVKs). Group comparison is case-sensitive
/// per Kubernetes naming rules; kind comparison is case-insensitive so users
/// who type `deployment` instead of `Deployment` still get a hit when the
/// builtin lookup couldn't normalize for them.
fn schema_matches_gvk(schema: &Value, gvk: &Gvk) -> bool {
    let Some(arr) = schema
        .get("x-kubernetes-group-version-kind")
        .and_then(Value::as_array)
    else {
        return false;
    };
    arr.iter().any(|entry| {
        let g = entry.get("group").and_then(Value::as_str).unwrap_or("");
        let v = entry.get("version").and_then(Value::as_str).unwrap_or("");
        let k = entry.get("kind").and_then(Value::as_str).unwrap_or("");
        g == gvk.group && v == gvk.version && k.eq_ignore_ascii_case(&gvk.kind)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parse_qualified_three_parts() {
        let g = parse_qualified("apps/v1/Deployment").unwrap();
        assert_eq!(g.group, "apps");
        assert_eq!(g.version, "v1");
        assert_eq!(g.kind, "Deployment");
    }

    #[test]
    fn parse_qualified_two_parts_is_core() {
        let g = parse_qualified("v1/Pod").unwrap();
        assert_eq!(g.group, "");
        assert_eq!(g.version, "v1");
        assert_eq!(g.kind, "Pod");
    }

    #[test]
    fn parse_qualified_plain_returns_none() {
        assert!(parse_qualified("Deployment").is_none());
    }

    #[test]
    fn parse_qualified_empty_segments_rejected() {
        assert!(parse_qualified("/v1/Pod").is_none());
        assert!(parse_qualified("apps//Deployment").is_none());
        assert!(parse_qualified("apps/v1/").is_none());
    }

    #[test]
    fn openapi_path_key_core_vs_grouped() {
        assert_eq!(openapi_path_key("", "v1"), "api/v1");
        assert_eq!(openapi_path_key("apps", "v1"), "apis/apps/v1");
        assert_eq!(
            openapi_path_key("networking.k8s.io", "v1"),
            "apis/networking.k8s.io/v1"
        );
    }

    #[test]
    fn schema_matches_gvk_finds_tagged_entry() {
        let schema = json!({
            "type": "object",
            "x-kubernetes-group-version-kind": [
                {"group": "apps", "version": "v1", "kind": "Deployment"}
            ]
        });
        let gvk = Gvk {
            group: "apps".into(),
            version: "v1".into(),
            kind: "Deployment".into(),
        };
        assert!(schema_matches_gvk(&schema, &gvk));
    }

    #[test]
    fn schema_matches_gvk_kind_is_case_insensitive() {
        let schema = json!({
            "x-kubernetes-group-version-kind": [
                {"group": "", "version": "v1", "kind": "Pod"}
            ]
        });
        let gvk = Gvk {
            group: "".into(),
            version: "v1".into(),
            kind: "pod".into(),
        };
        assert!(schema_matches_gvk(&schema, &gvk));
    }

    #[test]
    fn schema_matches_gvk_rejects_wrong_group() {
        let schema = json!({
            "x-kubernetes-group-version-kind": [
                {"group": "extensions", "version": "v1beta1", "kind": "Ingress"}
            ]
        });
        let gvk = Gvk {
            group: "networking.k8s.io".into(),
            version: "v1".into(),
            kind: "Ingress".into(),
        };
        assert!(!schema_matches_gvk(&schema, &gvk));
    }

    #[test]
    fn schema_matches_gvk_handles_missing_annotation() {
        let schema = json!({"type": "object"});
        let gvk = Gvk {
            group: "apps".into(),
            version: "v1".into(),
            kind: "Deployment".into(),
        };
        assert!(!schema_matches_gvk(&schema, &gvk));
    }

    #[test]
    fn schema_matches_gvk_walks_multi_tagged_array() {
        // Some schemas (e.g. shared list types) tag multiple GVKs.
        let schema = json!({
            "x-kubernetes-group-version-kind": [
                {"group": "apps", "version": "v1", "kind": "DeploymentList"},
                {"group": "apps", "version": "v1beta1", "kind": "DeploymentList"}
            ]
        });
        let gvk = Gvk {
            group: "apps".into(),
            version: "v1beta1".into(),
            kind: "DeploymentList".into(),
        };
        assert!(schema_matches_gvk(&schema, &gvk));
    }
}
