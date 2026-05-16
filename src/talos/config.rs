//! Resolve `talosctl` connection details from a talosconfig YAML file.
//!
//! Discovery order matches `talosctl`'s own behavior:
//!
//! 1. The path passed via `--talosconfig` (highest priority).
//! 2. The `TALOSCONFIG` environment variable.
//! 3. `~/.talos/config` (the default Talos puts the bootstrap config in).
//!
//! A talosconfig YAML looks roughly like this:
//!
//! ```yaml
//! context: mycluster
//! contexts:
//!   mycluster:
//!     endpoints:
//!       - 192.168.1.10
//!       - 192.168.1.11
//!     nodes:
//!       - 192.168.1.10
//!       - 192.168.1.11
//!       - 192.168.1.12
//!     ca:  <base64 PEM>
//!     crt: <base64 PEM>
//!     key: <base64 PEM>
//! ```
//!
//! The `endpoints` list is the apiserver/control-plane talosd endpoints.
//! The `nodes` list is every node `talosctl` should target by default. We
//! prefer `nodes` for fan-out because that's where the cert/data file paths
//! live; `endpoints` is what `talosctl` opens TCP connections to.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::Deserialize;

/// One resolved talosconfig: which path on disk we used and which nodes the
/// active context covers. `endpoints` (the apiserver/control-plane talosd
/// endpoints) is intentionally not surfaced yet — `certs`/`read`/`get` all
/// fan out across `nodes`. Add the field back when a control-plane-only
/// command (e.g. `etcd members`) needs it.
#[derive(Debug, Clone)]
pub struct TalosConfig {
    pub path: PathBuf,
    pub context: String,
    pub nodes: Vec<String>,
}

/// Resolve the path to use for `talosctl --talosconfig`. Search order is
/// flag → `TALOSCONFIG` → `~/.talos/config`. Returns an error only if no
/// candidate path exists.
pub fn resolve_path(flag: Option<&Path>) -> Result<PathBuf> {
    if let Some(p) = flag {
        return Ok(p.to_path_buf());
    }
    if let Ok(env) = std::env::var("TALOSCONFIG")
        && !env.is_empty()
    {
        return Ok(PathBuf::from(env));
    }
    if let Some(home) = std::env::var_os("HOME") {
        let default = Path::new(&home).join(".talos").join("config");
        if default.exists() {
            return Ok(default);
        }
    }
    bail!(
        "no talosconfig found — pass --talosconfig <path>, set $TALOSCONFIG, or place one at ~/.talos/config"
    )
}

/// Parse `path` as a talosconfig YAML and return the active context's
/// connection details.
pub fn load(path: &Path) -> Result<TalosConfig> {
    let bytes = std::fs::read(path)
        .with_context(|| format!("cannot read talosconfig: {}", path.display()))?;
    parse_inner(path, &bytes)
}

fn parse_inner(path: &Path, bytes: &[u8]) -> Result<TalosConfig> {
    #[derive(Deserialize)]
    struct Raw {
        context: Option<String>,
        contexts: std::collections::BTreeMap<String, Context>,
    }
    #[derive(Deserialize)]
    struct Context {
        #[serde(default)]
        nodes: Vec<String>,
    }

    let raw: Raw = serde_yaml::from_slice(bytes)
        .with_context(|| format!("invalid talosconfig YAML: {}", path.display()))?;

    let context_name = raw
        .context
        .clone()
        .or_else(|| raw.contexts.keys().next().cloned())
        .with_context(|| format!("talosconfig has no contexts: {}", path.display()))?;

    let ctx = raw
        .contexts
        .get(&context_name)
        .with_context(|| format!("active context `{}` not in contexts map", context_name))?;

    Ok(TalosConfig {
        path: path.to_path_buf(),
        context: context_name,
        nodes: ctx.nodes.clone(),
    })
}

/// Resolve a `--node <SPEC>` argument against the loaded config.
///
/// `None` (no flag) returns every node in the active context — the
/// fan-out default for `read`/`get`/`certs`. `Some("all")` is equivalent.
/// `Some("<ip>")` returns just that one — *no* validation that the IP is
/// in the context's node list, because users sometimes target nodes that
/// aren't in their default talosconfig (e.g. a worker reachable from the
/// control-plane endpoints).
pub fn resolve_nodes(cfg: &TalosConfig, spec: Option<&str>) -> Vec<String> {
    match spec {
        None | Some("all") => cfg.nodes.clone(),
        Some(other) => other.split(',').map(|s| s.trim().to_string()).collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "context: mycluster\n\
contexts:\n  mycluster:\n    endpoints:\n      - 192.168.1.10\n      - 192.168.1.11\n    \
nodes:\n      - 192.168.1.10\n      - 192.168.1.11\n      - 192.168.1.12\n";

    #[test]
    fn parses_active_context() {
        let cfg = parse_inner(Path::new("/dev/null"), SAMPLE.as_bytes()).unwrap();
        assert_eq!(cfg.context, "mycluster");
        assert_eq!(cfg.nodes.len(), 3);
        assert_eq!(cfg.nodes[2], "192.168.1.12");
    }

    #[test]
    fn falls_back_to_only_context() {
        let yaml = "contexts:\n  only:\n    nodes:\n      - 1.2.3.4\n";
        let cfg = parse_inner(Path::new("/dev/null"), yaml.as_bytes()).unwrap();
        assert_eq!(cfg.context, "only");
    }

    #[test]
    fn rejects_unknown_active_context() {
        let yaml = "context: ghost\ncontexts:\n  real:\n    nodes: []\n";
        let err = parse_inner(Path::new("/dev/null"), yaml.as_bytes()).unwrap_err();
        assert!(err.to_string().contains("not in contexts map"));
    }

    #[test]
    fn resolve_nodes_default_is_all() {
        let cfg = parse_inner(Path::new("/dev/null"), SAMPLE.as_bytes()).unwrap();
        assert_eq!(resolve_nodes(&cfg, None), cfg.nodes);
        assert_eq!(resolve_nodes(&cfg, Some("all")), cfg.nodes);
    }

    #[test]
    fn resolve_nodes_explicit() {
        let cfg = parse_inner(Path::new("/dev/null"), SAMPLE.as_bytes()).unwrap();
        assert_eq!(resolve_nodes(&cfg, Some("9.9.9.9")), vec!["9.9.9.9"]);
        assert_eq!(
            resolve_nodes(&cfg, Some("1.1.1.1, 2.2.2.2")),
            vec!["1.1.1.1", "2.2.2.2"]
        );
    }
}
