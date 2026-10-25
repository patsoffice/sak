//! Agent-hook redirect rules for the `base64` domain.
//!
//! The `base64` CLI (encode/decode, `--decode`/`-d`, `--ignore-garbage`)
//! only ever reads: encode and decode are both pure computations over their
//! inputs, so every invocation redirects. Two rules so the message names the
//! right direction: decode flags first, then everything else as encode.

use crate::hook::rule::HookRule;

pub const HOOK_RULES: &[HookRule] = &[
    HookRule {
        tool: "base64",
        subcommand: &[],
        guard: Some(is_decode),
        message: "Use `sak base64 decode <file>` instead of `base64 -d` \
             (omit <file> to decode stdin).",
    },
    HookRule {
        tool: "base64",
        subcommand: &[],
        guard: None,
        message: "Use `sak base64 encode <file>` instead of `base64` \
             (omit <file> to encode stdin; `--wrap 76` for MIME lines).",
    },
];

/// Whether the args ask for decoding: `--decode`, or a short-flag cluster
/// carrying GNU's `-d` or macOS's `-D` (`-d`, `-di`, `-D`).
fn is_decode(args: &[String]) -> bool {
    args.iter().any(|a| {
        a == "--decode"
            || (a.starts_with('-')
                && !a.starts_with("--")
                && a[1..].chars().any(|c| c == 'd' || c == 'D'))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(xs: &[&str]) -> Vec<String> {
        xs.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn decode_flags_are_recognised() {
        assert!(is_decode(&args(&["-d", "f"])));
        assert!(is_decode(&args(&["--decode"])));
        assert!(is_decode(&args(&["-D"])));
        assert!(is_decode(&args(&["-di", "f"])));
        assert!(!is_decode(&args(&["f"])));
        assert!(!is_decode(&args(&["-w", "0", "f"])));
        assert!(!is_decode(&args(&["--wrap=0"])));
    }
}
