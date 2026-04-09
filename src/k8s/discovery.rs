//! Resolve a user-supplied kind string to a Kubernetes `ApiResource`.
//!
//! Two-tier strategy:
//!
//! 1. **Fast path**: a hardcoded table of common builtin shortnames
//!    (`po`/`pod`/`pods` → `core/v1/Pod`, `deploy` → `apps/v1/Deployment`,
//!    etc.). On a hit we call [`kube::discovery::oneshot::pinned_kind`], which
//!    issues a single GVK lookup against the cluster.
//! 2. **Slow path**: on a miss we fall back to a full `kube::Discovery::run`,
//!    which walks every group/version on the cluster. This is multi-second on
//!    big clusters with many CRDs, but it's the only way to handle CRD
//!    shortnames and arbitrary user-defined kinds.
//!
//! The fast-path table is unit-testable without a live cluster; the resolver
//! function is exercised manually against a real cluster.

use anyhow::{Context, Result, anyhow};
use kube::api::ApiResource;
use kube::core::GroupVersionKind;
use kube::discovery::ApiCapabilities;

/// A single builtin kind and the names by which it can be referenced.
struct Builtin {
    /// All accepted spellings (lowercase). Includes singular, plural, and
    /// `kubectl`'s short aliases.
    aliases: &'static [&'static str],
    /// API group; empty string for the core group.
    group: &'static str,
    version: &'static str,
    kind: &'static str,
}

/// Hardcoded fast-path table of the ~30 most common Kubernetes builtins.
///
/// Anything not in this list falls through to full discovery. CRDs are not
/// in this list and always take the slow path.
const BUILTINS: &[Builtin] = &[
    // core/v1
    Builtin {
        aliases: &["po", "pod", "pods"],
        group: "",
        version: "v1",
        kind: "Pod",
    },
    Builtin {
        aliases: &["svc", "service", "services"],
        group: "",
        version: "v1",
        kind: "Service",
    },
    Builtin {
        aliases: &["cm", "configmap", "configmaps"],
        group: "",
        version: "v1",
        kind: "ConfigMap",
    },
    Builtin {
        aliases: &["secret", "secrets"],
        group: "",
        version: "v1",
        kind: "Secret",
    },
    Builtin {
        aliases: &["ns", "namespace", "namespaces"],
        group: "",
        version: "v1",
        kind: "Namespace",
    },
    Builtin {
        aliases: &["no", "node", "nodes"],
        group: "",
        version: "v1",
        kind: "Node",
    },
    Builtin {
        aliases: &["pv", "persistentvolume", "persistentvolumes"],
        group: "",
        version: "v1",
        kind: "PersistentVolume",
    },
    Builtin {
        aliases: &["pvc", "persistentvolumeclaim", "persistentvolumeclaims"],
        group: "",
        version: "v1",
        kind: "PersistentVolumeClaim",
    },
    Builtin {
        aliases: &["ep", "endpoints"],
        group: "",
        version: "v1",
        kind: "Endpoints",
    },
    Builtin {
        aliases: &["sa", "serviceaccount", "serviceaccounts"],
        group: "",
        version: "v1",
        kind: "ServiceAccount",
    },
    Builtin {
        aliases: &["ev", "event", "events"],
        group: "",
        version: "v1",
        kind: "Event",
    },
    Builtin {
        aliases: &["rc", "replicationcontroller", "replicationcontrollers"],
        group: "",
        version: "v1",
        kind: "ReplicationController",
    },
    Builtin {
        aliases: &["limits", "limitrange", "limitranges"],
        group: "",
        version: "v1",
        kind: "LimitRange",
    },
    Builtin {
        aliases: &["quota", "resourcequota", "resourcequotas"],
        group: "",
        version: "v1",
        kind: "ResourceQuota",
    },
    // apps/v1
    Builtin {
        aliases: &["deploy", "deployment", "deployments"],
        group: "apps",
        version: "v1",
        kind: "Deployment",
    },
    Builtin {
        aliases: &["sts", "statefulset", "statefulsets"],
        group: "apps",
        version: "v1",
        kind: "StatefulSet",
    },
    Builtin {
        aliases: &["ds", "daemonset", "daemonsets"],
        group: "apps",
        version: "v1",
        kind: "DaemonSet",
    },
    Builtin {
        aliases: &["rs", "replicaset", "replicasets"],
        group: "apps",
        version: "v1",
        kind: "ReplicaSet",
    },
    // batch/v1
    Builtin {
        aliases: &["job", "jobs"],
        group: "batch",
        version: "v1",
        kind: "Job",
    },
    Builtin {
        aliases: &["cj", "cronjob", "cronjobs"],
        group: "batch",
        version: "v1",
        kind: "CronJob",
    },
    // networking.k8s.io/v1
    Builtin {
        aliases: &["ing", "ingress", "ingresses"],
        group: "networking.k8s.io",
        version: "v1",
        kind: "Ingress",
    },
    Builtin {
        aliases: &["netpol", "networkpolicy", "networkpolicies"],
        group: "networking.k8s.io",
        version: "v1",
        kind: "NetworkPolicy",
    },
    Builtin {
        aliases: &["ingressclass", "ingressclasses"],
        group: "networking.k8s.io",
        version: "v1",
        kind: "IngressClass",
    },
    // rbac.authorization.k8s.io/v1
    Builtin {
        aliases: &["role", "roles"],
        group: "rbac.authorization.k8s.io",
        version: "v1",
        kind: "Role",
    },
    Builtin {
        aliases: &["rolebinding", "rolebindings"],
        group: "rbac.authorization.k8s.io",
        version: "v1",
        kind: "RoleBinding",
    },
    Builtin {
        aliases: &["clusterrole", "clusterroles"],
        group: "rbac.authorization.k8s.io",
        version: "v1",
        kind: "ClusterRole",
    },
    Builtin {
        aliases: &["clusterrolebinding", "clusterrolebindings"],
        group: "rbac.authorization.k8s.io",
        version: "v1",
        kind: "ClusterRoleBinding",
    },
    // storage.k8s.io/v1
    Builtin {
        aliases: &["sc", "storageclass", "storageclasses"],
        group: "storage.k8s.io",
        version: "v1",
        kind: "StorageClass",
    },
    // autoscaling/v2
    Builtin {
        aliases: &["hpa", "horizontalpodautoscaler", "horizontalpodautoscalers"],
        group: "autoscaling",
        version: "v2",
        kind: "HorizontalPodAutoscaler",
    },
    // policy/v1
    Builtin {
        aliases: &["pdb", "poddisruptionbudget", "poddisruptionbudgets"],
        group: "policy",
        version: "v1",
        kind: "PodDisruptionBudget",
    },
    // apiextensions.k8s.io/v1
    Builtin {
        aliases: &[
            "crd",
            "customresourcedefinition",
            "customresourcedefinitions",
        ],
        group: "apiextensions.k8s.io",
        version: "v1",
        kind: "CustomResourceDefinition",
    },
];

/// Look up a builtin kind by any of its accepted aliases.
///
/// Comparison is case-insensitive. Returns `None` for unknown kinds, which
/// callers should treat as a signal to fall back to full discovery.
pub fn lookup_builtin(name: &str) -> Option<GroupVersionKind> {
    let lower = name.to_ascii_lowercase();
    for b in BUILTINS {
        if b.aliases.iter().any(|a| *a == lower) {
            return Some(GroupVersionKind {
                group: b.group.to_string(),
                version: b.version.to_string(),
                kind: b.kind.to_string(),
            });
        }
    }
    None
}

/// Resolve a user-supplied kind string to an `ApiResource` and its capabilities
/// (scope, verbs, ...) against the live cluster, using the fast-path table when
/// possible.
pub async fn resolve(client: &kube::Client, kind: &str) -> Result<(ApiResource, ApiCapabilities)> {
    // Fast path: hardcoded shortname → GVK → single-GVK lookup.
    if let Some(gvk) = lookup_builtin(kind) {
        let pair = kube::discovery::oneshot::pinned_kind(client, &gvk)
            .await
            .with_context(|| format!("failed to resolve {kind:?} via pinned discovery"))?;
        return Ok(pair);
    }

    // Slow path: full discovery, then linear search by kind/plural.
    let discovery = kube::Discovery::new(client.clone())
        .run()
        .await
        .context("failed to run cluster discovery")?;
    for group in discovery.groups() {
        for (ar, caps) in group.recommended_resources() {
            if ar.kind.eq_ignore_ascii_case(kind) || ar.plural.eq_ignore_ascii_case(kind) {
                return Ok((ar, caps));
            }
        }
    }

    Err(anyhow!(
        "unknown kind: {kind:?} (not in builtin shortname table and not found via cluster discovery)"
    ))
}

/// Walk the entire cluster discovery tree and return every `(ApiResource,
/// ApiCapabilities)` pair the apiserver exposes. Used by `sak k8s kinds` —
/// this is one of the few legitimate uses of full discovery (the user is
/// explicitly asking for everything).
pub async fn discover_all(client: &kube::Client) -> Result<Vec<(ApiResource, ApiCapabilities)>> {
    let discovery = kube::Discovery::new(client.clone())
        .run()
        .await
        .context("failed to run cluster discovery")?;
    let mut out = Vec::new();
    for group in discovery.groups() {
        for (ar, caps) in group.recommended_resources() {
            out.push((ar, caps));
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pod_aliases_resolve_to_core_v1_pod() {
        for alias in ["po", "pod", "pods", "Pod", "PODS"] {
            let gvk = lookup_builtin(alias).unwrap_or_else(|| panic!("alias {alias:?} missing"));
            assert_eq!(gvk.group, "");
            assert_eq!(gvk.version, "v1");
            assert_eq!(gvk.kind, "Pod");
        }
    }

    #[test]
    fn deployment_aliases_resolve_to_apps_v1() {
        for alias in ["deploy", "deployment", "deployments", "Deployment"] {
            let gvk = lookup_builtin(alias).unwrap();
            assert_eq!(gvk.group, "apps");
            assert_eq!(gvk.version, "v1");
            assert_eq!(gvk.kind, "Deployment");
        }
    }

    #[test]
    fn cronjob_short_alias() {
        let gvk = lookup_builtin("cj").unwrap();
        assert_eq!(gvk.group, "batch");
        assert_eq!(gvk.kind, "CronJob");
    }

    #[test]
    fn ingress_in_networking_group() {
        let gvk = lookup_builtin("ing").unwrap();
        assert_eq!(gvk.group, "networking.k8s.io");
        assert_eq!(gvk.kind, "Ingress");
    }

    #[test]
    fn unknown_kind_returns_none() {
        assert!(lookup_builtin("widget").is_none());
        assert!(lookup_builtin("").is_none());
    }

    #[test]
    fn no_duplicate_aliases() {
        // Catches typos where the same alias is accidentally registered for
        // two different kinds.
        let mut seen = std::collections::HashMap::<&str, &str>::new();
        for b in BUILTINS {
            for alias in b.aliases {
                if let Some(prev) = seen.insert(*alias, b.kind) {
                    panic!("alias {alias:?} registered for both {prev} and {}", b.kind);
                }
            }
        }
    }
}
