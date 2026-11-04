//! Agent-hook redirect rules for the `json` domain.
//!
//! `jq FILTER [FILE]` maps to `sak json query`. Both the file form
//! (`jq FILTER FILE`) and the stdin form (`jq FILTER`, `... | jq .`) redirect —
//! `sak json query` reads stdin via `-`, so a piped filter is just as
//! redirectable as a file argument. Only a bare `jq` (no filter) passes through.

use crate::hook::rule::HookRule;

pub const HOOK_RULES: &[HookRule] = &[
    HookRule {
        tool: "jq",
        subcommand: &[],
        guard: Some(jq_has_filter),
        message: "Use `sak json query <path> <file>` instead of `jq` \
             (pass `-` as <file> to read stdin, e.g. `cmd | sak json query <path> -`). \
             Other ops: keys, flatten, grep, length, paths, schema, type, validate, diff.",
    },
    // `python` doing JSON work shadows `sak json` the way `jq` does. Inline
    // code (`python -c …`) is already denied by the `fs` inline-program row,
    // which the engine consults first, so this row is what catches the
    // module form, `python -m json.tool`, plus any other invocation visibly
    // naming `json`. A `script.py` file's imports aren't on the command line,
    // so script runs stay invisible by design. Versioned names (`python3`,
    // `python3.14`) reach this row through the engine's `canonical_tool`.
    HookRule {
        tool: "python",
        subcommand: &[],
        guard: Some(python_imports_json),
        message: "Don't parse JSON with `python`; use \
             `sak json query <path> <file>` instead \
             (pass `-` as <file> to read stdin). \
             Other ops: keys, table, validate, diff.",
    },
];

/// `jq FILTER [FILE]` carries a filter positional whether it reads a file or
/// stdin, so any invocation with at least one positional redirects. A bare `jq`
/// (no filter) has nothing to redirect.
fn jq_has_filter(args: &[String]) -> bool {
    args.iter().any(|a| !a.starts_with('-'))
}

/// `python`/`python3` doing visible JSON work: any argument mentioning the
/// `json` module — `import json`, `from json import ...`, `json.load(s)`,
/// `json.tool` (including `-m json.tool`), `__import__("json")`. Matched
/// against the raw args (quotes included), so `-c "import json; ..."`
/// fires whichever way the script is quoted. No marker means no shadow:
/// `python -c "print(1)"` and `python script.py` pass through.
fn python_imports_json(args: &[String]) -> bool {
    args.iter().any(|a| {
        a.contains("import json")
            || a.contains("from json import")
            || a.contains("json.load")
            || a.contains("json.tool")
            || a.contains("__import__(\"json\")")
            || a.contains("__import__('json')")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn a(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn jq_guard_fires_on_any_filter() {
        // File reads still fire.
        assert!(jq_has_filter(&a(&[".name", "pkg.json"])));
        assert!(jq_has_filter(&a(&["-r", ".name", "pkg.json"])));
        // Filter-only (stdin) invocations now fire too — sak reads stdin via `-`.
        assert!(jq_has_filter(&a(&["."])));
        assert!(jq_has_filter(&a(&["-r", "."])));
        // A bare `jq` (no filter) has nothing to redirect.
        assert!(!jq_has_filter(&a(&[])));
        assert!(!jq_has_filter(&a(&["-r"])));
    }

    #[test]
    fn python_guard_fires_on_json_marker_only() {
        // Every visible-json spelling fires.
        assert!(python_imports_json(&a(&[
            "-c",
            "import json, sys; print(json.load(sys.stdin))",
        ])));
        assert!(python_imports_json(&a(&[
            "-c",
            "from json import loads; print(loads('{}'))",
        ])));
        assert!(python_imports_json(&a(&["-m", "json.tool", "data.json"])));
        assert!(python_imports_json(&a(&["-c", "x = __import__(\"json\")"])));
        // No marker, no shadow: plain scripts and file-driven runs pass.
        assert!(!python_imports_json(&a(&["-c", "print(1)"])));
        assert!(!python_imports_json(&a(&["script.py"])));
        assert!(!python_imports_json(&a(&[])));
    }
}
