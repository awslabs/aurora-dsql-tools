//! Grammar corpus oracle: walks `tests/grammar/{accept,reject,fixed}/` and
//! asserts dsql-lint behaviour matches each fixture's expectation.
//!
//! See docs/plans/2026-05-14-grammar-integration-design.md.

mod grammar_corpus;

use dsql_lint::lint_sql;

/// Walks the corpus and asserts dsql-lint matches each fixture's expectation.
/// Collects all failures into one report so a single grammar change that
/// invalidates many probes shows the full impact in one CI run.
#[test]
fn corpus_contract_test() {
    let fixtures = grammar_corpus::load_corpus();
    assert!(
        !fixtures.is_empty(),
        "corpus is empty — at least one fixture should exist"
    );

    let mut failures: Vec<String> = Vec::new();

    for fx in &fixtures {
        match fx.kind {
            grammar_corpus::FixtureKind::Accept | grammar_corpus::FixtureKind::Fixed => {
                let diags = lint_sql(&fx.body);
                if !diags.is_empty() {
                    failures.push(format!(
                        "{}: expected clean lint, got {} diagnostic(s):\n  {}",
                        fx.rel_path,
                        diags.len(),
                        diags
                            .iter()
                            .map(|d| format!("[{:?}] {}", d.rule, d.message))
                            .collect::<Vec<_>>()
                            .join("\n  ")
                    ));
                }
            }
            grammar_corpus::FixtureKind::Reject => {
                let diags = lint_sql(&fx.body);
                if diags.is_empty() {
                    failures.push(format!(
                        "{}: expected at least one diagnostic, got none. \
                         Either add a rule that catches this, or move the \
                         fixture to accept/ if it's now valid.",
                        fx.rel_path,
                    ));
                } else if let Some(expected_rule) = &fx.header.rule {
                    let observed: Vec<String> = diags
                        .iter()
                        .map(|d| format!("{:?}", d.rule))
                        .collect();
                    let expected_pretty = grammar_corpus::snake_to_pascal(expected_rule);
                    if !observed.iter().any(|r| r == &expected_pretty) {
                        failures.push(format!(
                            "{}: header says rule '{expected_rule}' (LintRule::{expected_pretty}) \
                             should fire, but observed rules were [{}].",
                            fx.rel_path,
                            observed.join(", ")
                        ));
                    }
                }
            }
        }
    }

    if !failures.is_empty() {
        panic!(
            "{} corpus fixture(s) failed:\n\n{}",
            failures.len(),
            failures.join("\n\n")
        );
    }
}
