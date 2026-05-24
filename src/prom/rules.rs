//! `sak prom rules [--firing]` — list recording and alerting rules.
//!
//! Queries `/api/v1/rules` and walks every group's `rules` array, rendering
//! one rule per line as
//! `type<TAB>state<TAB>health<TAB>group<TAB>name<TAB>query`, sorted by
//! `(group, name, query)` for determinism.
//!
//! Recording rules have no alert `state`, so theirs renders as `-`.
//! `--firing` narrows to alerting rules currently in the `firing` state
//! (which excludes every recording rule, since they have no state).
//! Multi-line `query` expressions collapse to one row.

use std::process::ExitCode;

use anyhow::{Result, anyhow};
use clap::Args;
use serde_json::Value;

use crate::prom::common_args::CommonPromArgs;
use crate::prom::output::collapse_newlines;
use crate::prom::runner::run_prom;

#[derive(Args)]
#[command(
    about = "List recording and alerting rules",
    long_about = "List rules from `/api/v1/rules`, walking every rule group. \
        Each rule is one line: \
        `type<TAB>state<TAB>health<TAB>group<TAB>name<TAB>query`. Recording \
        rules have no alert state, shown as `-`.\n\n\
        Use --firing to narrow to alerting rules currently firing.\n\n\
        Connection: pass --url <http://prom:9090> or set PROMETHEUS_URL.",
    after_help = "\
Examples:
  sak prom rules                                All rules, all groups
  sak prom rules --firing                       Only currently-firing rules
  sak prom rules --json                         Raw JSON for piping"
)]
pub struct RulesArgs {
    #[command(flatten)]
    pub common: CommonPromArgs,

    /// Show only alerting rules currently in the `firing` state
    #[arg(long)]
    pub firing: bool,
}

/// One row extracted from a rule. Pure data so the group-walking and
/// formatting logic is unit-testable on hand-built fixtures. `Debug` so
/// `extract_rule_rows(...).unwrap_err()` works in the tests.
#[derive(Debug)]
pub(super) struct RuleRow {
    pub rule_type: String,
    pub state: String,
    pub health: String,
    pub group: String,
    pub name: String,
    pub query: String,
}

/// Walk every group's `rules` array, flattening into one [`RuleRow`] per
/// rule. Errors if the top-level `groups` array is missing; a group with
/// no `rules` array is skipped (treated as empty) rather than failing the
/// whole listing.
pub(super) fn extract_rule_rows(data: &Value) -> Result<Vec<RuleRow>> {
    let groups = data
        .get("groups")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("Prometheus /api/v1/rules data has no `groups` array"))?;
    let mut rows = Vec::new();
    for group in groups {
        let group_name = group.get("name").and_then(Value::as_str).unwrap_or("-");
        let Some(rules) = group.get("rules").and_then(Value::as_array) else {
            continue;
        };
        for rule in rules {
            rows.push(extract_rule_row(rule, group_name));
        }
    }
    Ok(rows)
}

/// Pull a row from a single rule object. Recording rules legitimately have
/// no `state` field — that renders as `-` rather than being treated as an
/// error. The raw `query` is stored verbatim; newline-collapsing happens at
/// format time.
fn extract_rule_row(rule: &Value, group_name: &str) -> RuleRow {
    RuleRow {
        rule_type: str_or(rule.get("type"), "-"),
        state: str_or(rule.get("state"), "-"),
        health: str_or(rule.get("health"), "-"),
        group: group_name.to_string(),
        name: str_or(rule.get("name"), "-"),
        query: str_or(rule.get("query"), ""),
    }
}

fn str_or(v: Option<&Value>, default: &str) -> String {
    v.and_then(Value::as_str).unwrap_or(default).to_string()
}

/// Format one row as the
/// `type<TAB>state<TAB>health<TAB>group<TAB>name<TAB>query` line. The
/// `query` is newline-collapsed so each rule stays one output row.
pub(super) fn format_rule_row(row: &RuleRow) -> String {
    format!(
        "{}\t{}\t{}\t{}\t{}\t{}",
        row.rule_type,
        row.state,
        row.health,
        row.group,
        row.name,
        collapse_newlines(&row.query)
    )
}

/// Sort by `(group, name, query)` for deterministic output. `query` is the
/// final tiebreaker because a group can legitimately hold two rules with
/// the same name (e.g. an alerting and a recording rule).
pub(super) fn sort_rows(rows: &mut [RuleRow]) {
    rows.sort_by(|a, b| {
        a.group
            .cmp(&b.group)
            .then_with(|| a.name.cmp(&b.name))
            .then_with(|| a.query.cmp(&b.query))
    });
}

pub fn run(args: &RulesArgs) -> Result<ExitCode> {
    run_prom(&args.common, "/api/v1/rules", |data| {
        let mut rows = extract_rule_rows(data)?;
        if args.firing {
            rows.retain(|r| r.state == "firing");
        }
        sort_rows(&mut rows);
        Ok(rows.iter().map(format_rule_row).collect())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn sample_rules() -> Value {
        json!({
            "groups": [
                {
                    "name": "node.rules",
                    "rules": [
                        {
                            "type": "recording",
                            "name": "instance:node_cpu:rate",
                            "query": "rate(node_cpu_seconds_total[5m])",
                            "health": "ok"
                        },
                        {
                            "type": "alerting",
                            "name": "NodeDown",
                            "query": "up == 0",
                            "state": "firing",
                            "health": "ok"
                        }
                    ]
                },
                {
                    "name": "empty.group"
                }
            ]
        })
    }

    #[test]
    fn extract_walks_all_groups_and_flattens() {
        let rows = extract_rule_rows(&sample_rules()).unwrap();
        assert_eq!(rows.len(), 2);
        assert!(rows.iter().all(|r| r.group == "node.rules"));
    }

    #[test]
    fn extract_recording_rule_has_dash_state() {
        let rows = extract_rule_rows(&sample_rules()).unwrap();
        let rec = rows.iter().find(|r| r.rule_type == "recording").unwrap();
        assert_eq!(rec.state, "-");
        assert_eq!(rec.name, "instance:node_cpu:rate");
        assert_eq!(rec.health, "ok");
    }

    #[test]
    fn extract_alerting_rule_keeps_state() {
        let rows = extract_rule_rows(&sample_rules()).unwrap();
        let alert = rows.iter().find(|r| r.rule_type == "alerting").unwrap();
        assert_eq!(alert.state, "firing");
        assert_eq!(alert.name, "NodeDown");
    }

    #[test]
    fn extract_errors_when_groups_missing() {
        let err = extract_rule_rows(&json!({})).unwrap_err();
        assert!(format!("{err}").contains("`groups` array"));
    }

    #[test]
    fn extract_skips_group_without_rules() {
        // `empty.group` in the fixture has no `rules` key — it must not
        // panic or error, just contribute zero rows.
        let rows = extract_rule_rows(&sample_rules()).unwrap();
        assert!(rows.iter().all(|r| r.group != "empty.group"));
    }

    #[test]
    fn extract_missing_fields_use_dashes() {
        let data = json!({"groups": [{"name": "g", "rules": [{}]}]});
        let rows = extract_rule_rows(&data).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].rule_type, "-");
        assert_eq!(rows[0].state, "-");
        assert_eq!(rows[0].health, "-");
        assert_eq!(rows[0].name, "-");
        assert_eq!(rows[0].query, "");
    }

    #[test]
    fn format_emits_tab_separated_line() {
        let row = RuleRow {
            rule_type: "alerting".into(),
            state: "firing".into(),
            health: "ok".into(),
            group: "node.rules".into(),
            name: "NodeDown".into(),
            query: "up == 0".into(),
        };
        assert_eq!(
            format_rule_row(&row),
            "alerting\tfiring\tok\tnode.rules\tNodeDown\tup == 0"
        );
    }

    #[test]
    fn format_collapses_multiline_query() {
        let row = RuleRow {
            rule_type: "recording".into(),
            state: "-".into(),
            health: "ok".into(),
            group: "g".into(),
            name: "r".into(),
            query: "sum(\n  rate(x[5m])\n)".into(),
        };
        assert!(format_rule_row(&row).contains("sum(   rate(x[5m]) )"));
    }

    #[test]
    fn sort_orders_by_group_then_name_then_query() {
        let mut rows = vec![
            rule("b.group", "Z", "q1"),
            rule("a.group", "Y", "q2"),
            rule("a.group", "Y", "q1"),
        ];
        sort_rows(&mut rows);
        assert_eq!(
            rows.iter()
                .map(|r| (r.group.as_str(), r.name.as_str(), r.query.as_str()))
                .collect::<Vec<_>>(),
            vec![
                ("a.group", "Y", "q1"),
                ("a.group", "Y", "q2"),
                ("b.group", "Z", "q1"),
            ]
        );
    }

    fn rule(group: &str, name: &str, query: &str) -> RuleRow {
        RuleRow {
            rule_type: "alerting".into(),
            state: "inactive".into(),
            health: "ok".into(),
            group: group.into(),
            name: name.into(),
            query: query.into(),
        }
    }
}
