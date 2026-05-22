//! Shared output helpers for the prom domain.
//!
//! Every `sak prom` command supports `--json` (a raw pretty-printed
//! passthrough of the upstream response) and most render free-text fields
//! that may contain newlines. Both the `--json` `BoundedWriter` dance
//! ([`crate::output::emit_json`]) and the newline-collapsing helper
//! ([`crate::output::collapse_newlines`]) now live in `crate::output`, shared
//! with `k8s`/`lxc`/`docker`. They're re-exported here so the prom command
//! files keep importing them from their own domain's output module.
pub(super) use crate::output::{collapse_newlines, emit_json};
