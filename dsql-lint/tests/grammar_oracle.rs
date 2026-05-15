//! Grammar oracle: test-time recognizer for `dsql_grammar.ebnf`.
//!
//! Loads the upstream EBNF, builds a chumsky recognizer, and asserts
//! dsql-lint and the recognizer agree on every test case. Disagreements
//! tracked in an `EXPECTED_DRIFT` const (see Phase 4); the list shrinks
//! over time as rules are added.

#[path = "grammar_oracle/mod.rs"]
mod grammar_oracle;

use grammar_oracle::drift;
use std::collections::HashSet;

#[test]
fn dsql_lint_agrees_with_grammar() {
    let disagreements = drift::collect();
    let expected: HashSet<&str> = drift::EXPECTED_DRIFT.iter().copied().collect();

    let unexpected: Vec<&drift::Disagreement> = disagreements
        .iter()
        .filter(|d| !expected.contains(d.sql.as_str()))
        .collect();

    if !unexpected.is_empty() {
        let mut msg =
            String::from("dsql-lint and grammar disagree on cases not in EXPECTED_DRIFT:\n\n");
        for d in &unexpected {
            msg.push_str(&format!("  [{:?}] {}\n", d.kind, d.sql));
        }
        msg.push_str(
            "\nFix by one of:\n  \
             - Add a rule to dsql-lint (most common)\n  \
             - Fix the recognizer if it's wrong\n  \
             - Add the SQL string to EXPECTED_DRIFT in tests/grammar_oracle/drift.rs \
               with a comment explaining why\n",
        );
        panic!("{msg}");
    }

    let actual: HashSet<&str> = disagreements.iter().map(|d| d.sql.as_str()).collect();
    let stale: Vec<&str> = drift::EXPECTED_DRIFT
        .iter()
        .copied()
        .filter(|s| !actual.contains(s))
        .collect();
    if !stale.is_empty() {
        let mut msg = String::from("EXPECTED_DRIFT contains entries that no longer disagree:\n");
        for s in &stale {
            msg.push_str(&format!("  {s}\n"));
        }
        msg.push_str("\nRemove these entries — they're stale.\n");
        panic!("{msg}");
    }
}
