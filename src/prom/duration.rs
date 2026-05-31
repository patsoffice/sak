//! Duration parsing for the prom domain — shared by `query-range`
//! (`--since`, `--step`) and `histogram` (`--rate-window`).
//!
//! The implementation was promoted to the crate-root [`crate::duration`]
//! module so the `docker` domain can reuse it without pulling in the `prom`
//! cargo feature. This module re-exports it so existing
//! `crate::prom::duration::parse_duration` call sites stay unchanged.

pub use crate::duration::parse_duration;
