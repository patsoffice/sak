//! Agent-hook redirect rules for the `sqlite` domain.
//!
//! One rule covers `sqlite3` — guard [`sqlite_has_read_marker`] looks for
//! the read meta-commands (`.tables`, `.schema`, `.dump`, `.indexes`,
//! `.databases`) or a `SELECT ` statement anywhere in the args.
//! Mutating SQL (`INSERT`, `UPDATE`, `CREATE TABLE`, ...) and the writable
//! interactive prompt pass through — sak sqlite is strictly read-only and
//! can't perform them. Match is case-insensitive (sqlite's own SQL parser
//! is too).
//!
//! `#[cfg(feature = "sqlite")]` gated in [`crate::hook::claude_code`]'s
//! `registries()` — a `--no-default-features` build drops these rules.

use crate::hook::rule::HookRule;

pub const HOOK_RULES: &[HookRule] = &[HookRule {
    tool: "sqlite3",
    subcommand: &[],
    guard: Some(sqlite_has_read_marker),
    message: "Use `sak sqlite tables/schema/count/query/dump/info <db>` \
         instead of `sqlite3` for reads.",
}];

/// `sqlite3` invoked with a read-only meta-command or `SELECT` query
/// anywhere in its args. Written/computed lower-case so the substring check
/// matches `SELECT`, `Select`, etc. — sqlite's own SQL parser is
/// case-insensitive on keywords.
fn sqlite_has_read_marker(args: &[String]) -> bool {
    const MARKERS: &[&str] = &[
        ".tables",
        ".schema",
        ".dump",
        ".indexes",
        ".databases",
        "select ",
    ];
    let joined = args.join(" ").to_lowercase();
    MARKERS.iter().any(|m| joined.contains(m))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn a(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn read_markers_match_case_insensitively() {
        assert!(sqlite_has_read_marker(&a(&["db.sqlite", ".tables"])));
        assert!(sqlite_has_read_marker(&a(&["db.sqlite", ".schema"])));
        assert!(sqlite_has_read_marker(&a(&["db.sqlite", ".dump", "users"])));
        assert!(sqlite_has_read_marker(&a(&[
            "db.sqlite",
            "SELECT * FROM users"
        ])));
        assert!(sqlite_has_read_marker(&a(&[
            "db.sqlite",
            "select * from users"
        ])));
    }

    #[test]
    fn mutations_and_bare_invocations_do_not_match() {
        assert!(!sqlite_has_read_marker(&a(&["db.sqlite"])));
        assert!(!sqlite_has_read_marker(&a(&[
            "db.sqlite",
            "INSERT INTO users VALUES (1)"
        ])));
        assert!(!sqlite_has_read_marker(&a(&[
            "db.sqlite",
            "CREATE TABLE x (a INT)"
        ])));
    }
}
