//! Agent-hook redirect rules for the `cert` domain.
//!
//! `openssl` is shared between `cert` (the `x509` and `verify` subcommands
//! here) and `hash` (the `dgst` subcommand, declared in [`crate::hash::hook`]).
//! The rules have disjoint subcommands so they can't collide in the engine,
//! and they sit beside the commands they shadow rather than being clumped
//! together.

use crate::hook::rule::HookRule;

pub const HOOK_RULES: &[HookRule] = &[
    HookRule {
        tool: "openssl",
        subcommand: &[&["x509"]],
        guard: None,
        message: "Use `sak cert inspect <cert>` instead of `openssl x509` \
             (omit <cert> to read PEM/DER from stdin, e.g. `cat cert.pem | sak cert inspect`). \
             Also: `sak cert expiring --days 30`, `sak cert from-kubeconfig`.",
    },
    HookRule {
        tool: "openssl",
        subcommand: &[&["verify"]],
        guard: Some(verify_is_redirectable),
        message: "Use `sak cert verify-chain --ca <CAfile> [--untrusted <file>] <cert>` \
             instead of `openssl verify -CAfile …` (or `sak cert verify-chain chain.pem` \
             for a complete leaf+inter+root bundle). With --ca, extra certs in \
             <cert> join --untrusted as intermediates — openssl reads only the first.",
    },
];

/// Whether an `openssl verify` invocation is one `sak cert verify-chain` can
/// answer: it names a `-CAfile` (openssl 3 accepts `-CAfile=x` too), and its
/// only other options are `-untrusted`, `-no-CApath` (the closest match to
/// sak, which never reads the system store), and the display-only
/// `-verbose` / `-show_chain`. Without `-CAfile` sak has no trust anchor to
/// check against (openssl would fall back to the system store, which sak
/// never reads); any other option (`-CApath`, `-crl_check`, `-purpose`,
/// `-attime`, …) asks for a check sak doesn't do. Redirecting those would
/// send the model to a command that can't answer the question. Note
/// `-CAfile` alone doesn't isolate openssl from the system store either
/// (that takes `-no-CApath`), so sak — which checks only the given
/// anchors — can disagree with openssl on chains the system store would
/// rescue.
fn verify_is_redirectable(args: &[String]) -> bool {
    let mut has_ca = false;
    let mut rest = args.iter().skip_while(|a| *a != "verify").skip(1);
    while let Some(arg) = rest.next() {
        // openssl accepts both `-opt` and `--opt`, and `-opt=value`.
        let body = arg.strip_prefix("--").or_else(|| arg.strip_prefix('-'));
        let Some(body) = body else {
            continue; // positional (a cert file)
        };
        let (opt, inline) = match body.split_once('=') {
            Some((o, _)) => (o, true),
            None => (body, false),
        };
        match opt {
            "CAfile" => {
                has_ca = true;
                if !inline {
                    rest.next();
                }
            }
            "untrusted" => {
                if !inline {
                    rest.next();
                }
            }
            "no-CApath" | "verbose" | "show_chain" => {}
            _ => return false,
        }
    }
    has_ca
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(s: &str) -> Vec<String> {
        s.split_whitespace().map(str::to_string).collect()
    }

    #[test]
    fn verify_redirects_only_what_verify_chain_can_answer() {
        assert!(verify_is_redirectable(&args(
            "verify -CAfile r.pem leaf.pem"
        )));
        assert!(verify_is_redirectable(&args(
            "verify -CAfile r.pem -untrusted i.pem leaf.pem"
        )));
        assert!(verify_is_redirectable(&args(
            "verify --CAfile r.pem leaf.pem"
        )));
        // openssl 3 `-opt=value` spelling, and display-only flags.
        assert!(verify_is_redirectable(&args(
            "verify -CAfile=r.pem leaf.pem"
        )));
        assert!(verify_is_redirectable(&args(
            "verify -CAfile r.pem -no-CApath -verbose leaf.pem"
        )));
        assert!(verify_is_redirectable(&args(
            "verify -CAfile r.pem -untrusted=i.pem -show_chain leaf.pem"
        )));
        // System trust store: sak has no equivalent.
        assert!(!verify_is_redirectable(&args("verify leaf.pem")));
        assert!(!verify_is_redirectable(&args(
            "verify -untrusted i.pem leaf.pem"
        )));
        // Checks sak doesn't do.
        assert!(!verify_is_redirectable(&args(
            "verify -CAfile r.pem -crl_check leaf.pem"
        )));
        assert!(!verify_is_redirectable(&args(
            "verify -CApath /etc/ssl leaf.pem"
        )));
        assert!(!verify_is_redirectable(&args(
            "verify -CAfile r.pem -purpose sslserver leaf.pem"
        )));
        // -attime takes epoch seconds; sak --at takes RFC3339.
        assert!(!verify_is_redirectable(&args(
            "verify -CAfile r.pem -attime 1893456000 leaf.pem"
        )));
    }
}
