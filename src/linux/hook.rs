//! Agent-hook redirect rules for the `linux` domain.
//!
//! Only `sysctl` is shadowed today (the `/proc`-based readers — `cpuinfo`,
//! `meminfo`, `mounts`, `loadavg`, `uptime`, `process`, `network` — have no
//! shell counterpart to redirect). The rule uses an empty `subcommand` plus
//! the [`sysctl_is_read`] guard because the read-vs-write split is flag- and
//! syntax-driven, not verb-driven.
//!
//! The `linux` domain has no `client.rs` chokepoint, so there's no name-token
//! exemption to worry about — `pub mod hook;` in `src/linux/mod.rs` is the
//! whole wiring.

use crate::hook::rule::HookRule;

pub const HOOK_RULES: &[HookRule] = &[HookRule {
    tool: "sysctl",
    subcommand: &[],
    guard: Some(sysctl_is_read),
    message: "Use `sak linux sysctl [pattern]` instead of `sysctl` for reads.",
}];

/// `sysctl` reading a knob: bare `sysctl`, `sysctl -a`, or `sysctl <key>`.
/// Mutations are out of scope for sak and pass through — setting a knob is
/// `key=value` (with or without `-w`/`--write`), and loading a config file is
/// `-p`/`--load`/`--system`. Any of those flags or any `key=value` token
/// declines.
fn sysctl_is_read(args: &[String]) -> bool {
    !args.iter().any(|a| {
        a.contains('=')
            || a == "-w"
            || a == "--write"
            || a == "-p"
            || a == "--load"
            || a == "--system"
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn a(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn sysctl_reads_are_distinguished_from_writes() {
        // Reads
        assert!(sysctl_is_read(&a(&[])));
        assert!(sysctl_is_read(&a(&["-a"])));
        assert!(sysctl_is_read(&a(&["--all"])));
        assert!(sysctl_is_read(&a(&["net.ipv4.tcp_syncookies"])));
        assert!(sysctl_is_read(&a(&["-n", "kernel.hostname"])));
        // Writes: key=value form (with or without -w) and config-load flags.
        assert!(!sysctl_is_read(&a(&["-w", "net.ipv4.ip_forward=1"])));
        assert!(!sysctl_is_read(&a(&["net.ipv4.ip_forward=1"])));
        assert!(!sysctl_is_read(&a(&["-p"])));
        assert!(!sysctl_is_read(&a(&["-p", "/etc/sysctl.conf"])));
        assert!(!sysctl_is_read(&a(&["--system"])));
    }
}
