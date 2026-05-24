//! A small builder for assembling the `gh` CLI argument vector.
//!
//! Every `sak gh <cmd>` turns its parsed args into the `Vec<String>` argv that
//! [`crate::gh::client::invoke_ok`] forwards to `gh`. The hand-rolled form —
//! `argv.push("--flag".into()); argv.push(value.clone());` repeated per option,
//! guarded by `if let Some(..)` / `for ..` — was near-identical across ~10
//! commands. [`ArgvBuilder`] collapses it to a fluent chain and keeps the
//! `to_string` conversions in one place so call sites carry no explicit
//! `.clone()`s.
//!
//! It is gh-domain-local on purpose: the shape (a flat positional + flag/value
//! argv) is specific to forwarding to an external CLI, not a general concern.

/// Accumulates a `gh` argv as an ordered `Vec<String>`.
pub(super) struct ArgvBuilder {
    argv: Vec<String>,
}

impl ArgvBuilder {
    pub(super) fn new() -> Self {
        Self { argv: Vec::new() }
    }

    /// Push a bare token: a positional argument (PR number, endpoint, tag) or
    /// a pre-built value with no preceding flag.
    pub(super) fn push_value(&mut self, value: impl Into<String>) -> &mut Self {
        self.argv.push(value.into());
        self
    }

    /// Push a valueless flag, e.g. `--all`.
    pub(super) fn push_flag(&mut self, flag: &str) -> &mut Self {
        self.argv.push(flag.to_string());
        self
    }

    /// Push a valueless flag only when `cond` holds.
    pub(super) fn push_flag_if(&mut self, cond: bool, flag: &str) -> &mut Self {
        if cond {
            self.push_flag(flag);
        }
        self
    }

    /// Push `flag` followed by `value` (two argv entries).
    pub(super) fn push(&mut self, flag: &str, value: &str) -> &mut Self {
        self.argv.push(flag.to_string());
        self.argv.push(value.to_string());
        self
    }

    /// Push `flag value` only when `value` is `Some`.
    pub(super) fn push_opt(&mut self, flag: &str, value: Option<&str>) -> &mut Self {
        if let Some(v) = value {
            self.push(flag, v);
        }
        self
    }

    /// Push `flag value` once per element — a repeatable flag (`--label`,
    /// `-H`, `-f`, ...). Order is preserved.
    pub(super) fn push_each(&mut self, flag: &str, values: &[String]) -> &mut Self {
        for v in values {
            self.push(flag, v);
        }
        self
    }

    /// Consume the builder, yielding the assembled argv.
    pub(super) fn into_argv(self) -> Vec<String> {
        self.argv
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_builder_is_empty() {
        assert!(ArgvBuilder::new().into_argv().is_empty());
    }

    #[test]
    fn ordering_and_variants() {
        let mut b = ArgvBuilder::new();
        b.push_value("123")
            .push("--json", "number,title")
            .push_opt("--repo", Some("cli/cli"))
            .push_opt("--author", None)
            .push_flag_if(true, "--all")
            .push_flag_if(false, "--draft")
            .push_each("--label", &["bug".to_string(), "p1".to_string()]);
        assert_eq!(
            b.into_argv(),
            vec![
                "123",
                "--json",
                "number,title",
                "--repo",
                "cli/cli",
                "--all",
                "--label",
                "bug",
                "--label",
                "p1",
            ]
        );
    }
}
