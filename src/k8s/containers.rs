//! Pure walker over the various pod-bearing resource shapes Kubernetes
//! exposes. Used by both `sak k8s images` and `sak k8s env`.
//!
//! Walker only — no networking, no `kube` types. Inputs are
//! `serde_json::Value`, so the walker is fully unit-testable on hand-built
//! fixtures with no cluster needed.
//!
//! Three shapes are recognized:
//!
//! - `Pod`:                                  `spec.containers[*]`
//! - Pod-template owners (Deployment,        `spec.template.spec.containers[*]`
//!   StatefulSet, DaemonSet, Job,
//!   ReplicaSet):
//! - `CronJob` (extra hop through            `spec.jobTemplate.spec.template.spec.containers[*]`
//!   `jobTemplate`):
//!
//! Anything else yields zero containers — callers that need to surface "this
//! kind is not supported" must do that check at the command layer, where they
//! have access to the resolved `ApiResource::kind`.
//!
//! `initContainers` and ephemeral containers are deliberately not walked yet —
//! the LLM-utility query is "what's actually running and being scheduled."
//! Add a flag if a real use case for init/ephemeral surfaces.

use serde_json::Value;

/// A single container, plus enough resource context to identify it in
/// downstream output. Borrows from the input `Value`; the lifetime is the
/// lifetime of that value.
pub struct ContainerView<'a> {
    pub namespace: Option<&'a str>,
    /// Resource name (the Pod/Deployment/etc., not the container).
    pub name: &'a str,
    pub container: &'a str,
    pub image: &'a str,
    /// The container's `env` array, or an empty slice if absent.
    pub env: &'a [Value],
}

/// Walk every container in `value`, yielding one [`ContainerView`] per entry.
///
/// Tries each known shape in order and stops at the first one that resolves
/// to a containers array — a single resource will only match one shape.
pub fn walk_containers(value: &Value) -> impl Iterator<Item = ContainerView<'_>> {
    let metadata = value.get("metadata");
    let namespace = metadata
        .and_then(|m| m.get("namespace"))
        .and_then(Value::as_str);
    let name = metadata
        .and_then(|m| m.get("name"))
        .and_then(Value::as_str)
        .unwrap_or("");

    // Ordered by specificity: CronJob's deeper path is tried first so a
    // CronJob doesn't accidentally match the Deployment shape on a partially
    // populated fixture. In practice each real resource only has one of these
    // paths populated, but being explicit is cheap.
    const SHAPES: &[&[&str]] = &[
        &[
            "spec",
            "jobTemplate",
            "spec",
            "template",
            "spec",
            "containers",
        ],
        &["spec", "template", "spec", "containers"],
        &["spec", "containers"],
    ];

    let mut containers: &[Value] = &[];
    for path in SHAPES {
        if let Some(arr) = resolve_path(value, path).and_then(Value::as_array) {
            containers = arr.as_slice();
            break;
        }
    }

    containers.iter().map(move |c| ContainerView {
        namespace,
        name,
        container: c.get("name").and_then(Value::as_str).unwrap_or(""),
        image: c.get("image").and_then(Value::as_str).unwrap_or(""),
        env: c
            .get("env")
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or(&[]),
    })
}

fn resolve_path<'a>(value: &'a Value, path: &[&str]) -> Option<&'a Value> {
    let mut current = value;
    for seg in path {
        current = current.get(seg)?;
    }
    Some(current)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn pod_shape_yields_each_container() {
        let pod = json!({
            "metadata": {"namespace": "default", "name": "web"},
            "spec": {
                "containers": [
                    {"name": "app", "image": "nginx:1.27"},
                    {"name": "sidecar", "image": "envoy:v1.31"}
                ]
            }
        });
        let v: Vec<_> = walk_containers(&pod).collect();
        assert_eq!(v.len(), 2);
        assert_eq!(v[0].namespace, Some("default"));
        assert_eq!(v[0].name, "web");
        assert_eq!(v[0].container, "app");
        assert_eq!(v[0].image, "nginx:1.27");
        assert_eq!(v[1].container, "sidecar");
        assert_eq!(v[1].image, "envoy:v1.31");
    }

    #[test]
    fn deployment_shape_walks_template_spec() {
        let deploy = json!({
            "metadata": {"namespace": "ns1", "name": "api"},
            "spec": {
                "template": {
                    "spec": {
                        "containers": [
                            {"name": "api", "image": "registry/api:1.0"}
                        ]
                    }
                }
            }
        });
        let v: Vec<_> = walk_containers(&deploy).collect();
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].namespace, Some("ns1"));
        assert_eq!(v[0].name, "api");
        assert_eq!(v[0].container, "api");
        assert_eq!(v[0].image, "registry/api:1.0");
    }

    #[test]
    fn cronjob_shape_walks_through_job_template() {
        let cj = json!({
            "metadata": {"namespace": "batch", "name": "nightly"},
            "spec": {
                "jobTemplate": {
                    "spec": {
                        "template": {
                            "spec": {
                                "containers": [
                                    {"name": "runner", "image": "tools:latest"}
                                ]
                            }
                        }
                    }
                }
            }
        });
        let v: Vec<_> = walk_containers(&cj).collect();
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].name, "nightly");
        assert_eq!(v[0].container, "runner");
        assert_eq!(v[0].image, "tools:latest");
    }

    #[test]
    fn unknown_shape_yields_nothing() {
        let svc = json!({
            "metadata": {"namespace": "default", "name": "svc"},
            "spec": {"selector": {"app": "x"}}
        });
        assert_eq!(walk_containers(&svc).count(), 0);
    }

    #[test]
    fn missing_metadata_uses_empty_name_and_no_namespace() {
        let pod = json!({
            "spec": {"containers": [{"name": "c", "image": "i"}]}
        });
        let v: Vec<_> = walk_containers(&pod).collect();
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].namespace, None);
        assert_eq!(v[0].name, "");
    }

    #[test]
    fn env_array_passed_through_when_present() {
        let pod = json!({
            "metadata": {"namespace": "ns", "name": "p"},
            "spec": {
                "containers": [{
                    "name": "c",
                    "image": "i",
                    "env": [
                        {"name": "FOO", "value": "bar"},
                        {"name": "REF", "valueFrom": {"secretKeyRef": {"name": "s", "key": "k"}}}
                    ]
                }]
            }
        });
        let v: Vec<_> = walk_containers(&pod).collect();
        assert_eq!(v[0].env.len(), 2);
        assert_eq!(v[0].env[0].get("name").and_then(Value::as_str), Some("FOO"));
    }

    #[test]
    fn env_absent_yields_empty_slice() {
        let pod = json!({
            "metadata": {"namespace": "ns", "name": "p"},
            "spec": {"containers": [{"name": "c", "image": "i"}]}
        });
        let v: Vec<_> = walk_containers(&pod).collect();
        assert!(v[0].env.is_empty());
    }

    #[test]
    fn cronjob_takes_precedence_over_deployment_shape() {
        // Pathologically constructed: both `spec.template.spec.containers`
        // and the deeper CronJob path resolve. The walker must prefer the
        // more specific (deeper) shape so a CronJob with a redundant inner
        // template still walks the right containers.
        let weird = json!({
            "metadata": {"namespace": "ns", "name": "x"},
            "spec": {
                "template": {"spec": {"containers": [{"name": "wrong", "image": "wrong"}]}},
                "jobTemplate": {"spec": {"template": {"spec": {
                    "containers": [{"name": "right", "image": "right"}]
                }}}}
            }
        });
        let v: Vec<_> = walk_containers(&weird).collect();
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].container, "right");
    }
}
