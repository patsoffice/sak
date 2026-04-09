//! `sak k8s env <kind> <name>` — list environment variables on a single
//! pod-bearing resource, one variable per line.
//!
//! **Read-only**: this command does *not* dereference `secretKeyRef` /
//! `configMapKeyRef` to fetch the actual values. It only reports the
//! references, by design — surfacing secret material would defeat the
//! purpose of a read-only LLM tool.

use std::io;
use std::process::ExitCode;

use anyhow::{Result, bail};
use clap::Args;
use serde_json::Value;

use crate::k8s::{client, containers, discovery};
use crate::output::BoundedWriter;

/// Kinds that have a pod template (and therefore env vars worth reporting).
/// Anything else gets a clear error at the front of the command.
const ALLOWED_KINDS: &[&str] = &[
    "Pod",
    "Deployment",
    "StatefulSet",
    "DaemonSet",
    "Job",
    "CronJob",
    "ReplicaSet",
];

#[derive(Args)]
#[command(
    about = "List env vars on a pod-bearing resource",
    long_about = "List the environment variables defined on every container of a single \
        Kubernetes resource. Restricted to kinds that have a pod template: \
        Pod, Deployment, StatefulSet, DaemonSet, Job, CronJob, ReplicaSet. \
        Anything else returns a clear error.\n\n\
        Output is `container<TAB>name<TAB>value-or-ref` per env var, where \
        `value-or-ref` is one of:\n\n  \
        - the literal string for `value:` entries\n  \
        - `cm:<configmap>/<key>` for configMapKeyRef\n  \
        - `secret:<secret>/<key>` for secretKeyRef\n  \
        - `field:<fieldPath>` for fieldRef\n  \
        - `resource:<resource>` for resourceFieldRef\n\n\
        Secret and ConfigMap references are *not* dereferenced — only the \
        reference is reported. This is intentional: a read-only LLM tool \
        should not surface secret material.",
    after_help = "\
Examples:
  sak k8s env pod web-7d8f9 -n default
  sak k8s env deployment api -n production
  sak k8s env cronjob nightly -n batch"
)]
pub struct EnvArgs {
    /// Resource kind (must have a pod template)
    pub kind: String,

    /// Resource name
    pub name: String,

    /// Namespace
    #[arg(short, long)]
    pub namespace: Option<String>,

    /// Maximum number of output lines
    #[arg(long)]
    pub limit: Option<usize>,
}

pub async fn run(args: &EnvArgs) -> Result<ExitCode> {
    let client = client::build_client().await?;
    let (ar, _caps) = discovery::resolve(&client, &args.kind).await?;

    if !ALLOWED_KINDS.contains(&ar.kind.as_str()) {
        bail!(
            "kind {:?} has no pod template — `sak k8s env` only supports: {}",
            ar.kind,
            ALLOWED_KINDS.join(", ")
        );
    }

    let ns = args
        .namespace
        .clone()
        .unwrap_or_else(|| client.default_namespace().to_string());

    let obj = client::get_dyn(&client, &ar, Some(ns.as_str()), &args.name).await?;
    let Some(obj) = obj else {
        // Not found → sak exit code 1, no stdout output.
        return Ok(ExitCode::from(1));
    };
    let value: Value = serde_json::to_value(&obj)?;

    let stdout = io::stdout();
    let handle = stdout.lock();
    let mut writer = BoundedWriter::new(handle, args.limit);

    let mut wrote_any = false;
    'outer: for view in containers::walk_containers(&value) {
        for entry in view.env {
            let Some(name) = entry.get("name").and_then(Value::as_str) else {
                continue;
            };
            let rendered = render_env_value(entry);
            let line = format!("{}\t{}\t{}", view.container, name, rendered);
            if !writer.write_line(&line)? {
                break 'outer;
            }
            wrote_any = true;
        }
    }

    writer.flush()?;
    if wrote_any {
        Ok(ExitCode::SUCCESS)
    } else {
        Ok(ExitCode::from(1))
    }
}

/// Render a single `env` array entry as the `value-or-ref` column.
///
/// Falls back to `?` for shapes we don't recognize so the output is always
/// well-formed and the user can tell something unusual is in the spec without
/// the command failing.
fn render_env_value(entry: &Value) -> String {
    if let Some(s) = entry.get("value").and_then(Value::as_str) {
        return s.to_string();
    }
    let Some(from) = entry.get("valueFrom") else {
        return "?".to_string();
    };

    if let Some(r) = from.get("configMapKeyRef") {
        let name = r.get("name").and_then(Value::as_str).unwrap_or("");
        let key = r.get("key").and_then(Value::as_str).unwrap_or("");
        return format!("cm:{}/{}", name, key);
    }
    if let Some(r) = from.get("secretKeyRef") {
        let name = r.get("name").and_then(Value::as_str).unwrap_or("");
        let key = r.get("key").and_then(Value::as_str).unwrap_or("");
        return format!("secret:{}/{}", name, key);
    }
    if let Some(r) = from.get("fieldRef") {
        let path = r.get("fieldPath").and_then(Value::as_str).unwrap_or("");
        return format!("field:{}", path);
    }
    if let Some(r) = from.get("resourceFieldRef") {
        let res = r.get("resource").and_then(Value::as_str).unwrap_or("");
        return format!("resource:{}", res);
    }
    "?".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn literal_value() {
        let e = json!({"name": "FOO", "value": "bar"});
        assert_eq!(render_env_value(&e), "bar");
    }

    #[test]
    fn configmap_ref() {
        let e = json!({
            "name": "X",
            "valueFrom": {"configMapKeyRef": {"name": "app-config", "key": "log-level"}}
        });
        assert_eq!(render_env_value(&e), "cm:app-config/log-level");
    }

    #[test]
    fn secret_ref() {
        let e = json!({
            "name": "X",
            "valueFrom": {"secretKeyRef": {"name": "db", "key": "password"}}
        });
        assert_eq!(render_env_value(&e), "secret:db/password");
    }

    #[test]
    fn field_ref() {
        let e = json!({
            "name": "POD_NAMESPACE",
            "valueFrom": {"fieldRef": {"fieldPath": "metadata.namespace"}}
        });
        assert_eq!(render_env_value(&e), "field:metadata.namespace");
    }

    #[test]
    fn resource_field_ref() {
        let e = json!({
            "name": "CPU_LIMIT",
            "valueFrom": {"resourceFieldRef": {"resource": "limits.cpu"}}
        });
        assert_eq!(render_env_value(&e), "resource:limits.cpu");
    }

    #[test]
    fn unknown_shape_yields_question_mark() {
        let e = json!({"name": "X", "valueFrom": {"weirdRef": {}}});
        assert_eq!(render_env_value(&e), "?");
    }
}
