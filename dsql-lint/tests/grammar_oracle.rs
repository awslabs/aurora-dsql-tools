//! Grammar oracle: test-time check that dsql-lint and the upstream
//! grammar agree on each statement in our test corpora.
//!
//! Loads `dsql_grammar.json` (Snowglobe-shaped grammar; see
//! `grammar_oracle/snowglobe.rs`) and asserts dsql-lint's verdicts
//! match the grammar's on every case. Disagreements from the curated
//! dsql-lint corpus are gated per-statement by `EXPECTED_DRIFT` in
//! `grammar_oracle/drift.rs`; that list shrinks as rules are added.
//! Disagreements from the larger Postgres regression corpus are
//! reported but don't fail CI — they're the burndown surface for
//! future rule-gap triage.

#[path = "grammar_oracle/mod.rs"]
mod grammar_oracle;

use grammar_oracle::drift;
use std::collections::HashSet;

#[test]
fn dsql_lint_agrees_with_grammar() {
    let disagreements = drift::collect();
    let expected: HashSet<&str> = drift::EXPECTED_DRIFT.iter().map(|(s, _)| *s).collect();

    // Disagreements from the dsql-lint corpus are gated by per-statement
    // EXPECTED_DRIFT entries. Anything not in the list is a new disagreement.
    let unexpected_dsql_lint: Vec<&drift::Disagreement> = disagreements
        .iter()
        .filter(|d| d.source == drift::CorpusSource::DsqlLint)
        .filter(|d| !expected.contains(d.sql.as_str()))
        .collect();

    // Disagreements from the Postgres corpus are predicate-filtered in
    // `drift::collect`; anything that survives is a real lint-rule gap
    // (SQL the grammar correctly rejects but dsql-lint silently accepts —
    // or vice versa). Surface the full list, capped so the test message
    // stays readable.
    let pg_disagreements: Vec<&drift::Disagreement> = disagreements
        .iter()
        .filter(|d| d.source == drift::CorpusSource::Pg)
        .collect();

    if !unexpected_dsql_lint.is_empty() {
        let mut msg =
            String::from("dsql-lint and grammar disagree on cases not in EXPECTED_DRIFT:\n\n");
        for d in &unexpected_dsql_lint {
            msg.push_str(&format!("  [{:?}] {}\n", d.kind, d.sql));
        }
        msg.push_str(
            "\nFix by one of:\n  \
             - Add a rule to dsql-lint (DriftReason::MissingDsqlLintRule)\n  \
             - Extend the recognizer (DriftReason::RecognizerHole)\n  \
             - Add the SQL to EXPECTED_DRIFT in tests/grammar_oracle/drift.rs \
               with a (sql, DriftReason::*) entry and a comment explaining why\n",
        );
        panic!("{msg}");
    }

    let actual: HashSet<&str> = disagreements
        .iter()
        .filter(|d| d.source == drift::CorpusSource::DsqlLint)
        .map(|d| d.sql.as_str())
        .collect();
    let stale: Vec<&str> = drift::EXPECTED_DRIFT
        .iter()
        .map(|(s, _)| *s)
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

    // PG corpus disagreements are reported but don't fail CI yet: this is
    // the burndown surface. Print a summary so a `cargo test -- --nocapture`
    // run shows what's outstanding.
    if !pg_disagreements.is_empty() {
        eprintln!(
            "\nPg corpus drift: {} statement(s) — see drift report.",
            pg_disagreements.len()
        );
        let by_kind = |kind: drift::DisagreementKind| -> usize {
            pg_disagreements.iter().filter(|d| d.kind == kind).count()
        };
        eprintln!(
            "  LintFlagsGrammarAccepts: {}\n  GrammarRejectsLintQuiet: {}",
            by_kind(drift::DisagreementKind::LintFlagsGrammarAccepts),
            by_kind(drift::DisagreementKind::GrammarRejectsLintQuiet),
        );
    }
}

/// Prints the full Postgres-corpus drift report. Run with `cargo test
/// -p dsql-lint --test grammar_oracle pg_corpus_drift_report --
/// --nocapture` to see the burndown surface.
#[test]
fn pg_corpus_drift_report() {
    let disagreements = drift::collect();
    let pg: Vec<&drift::Disagreement> = disagreements
        .iter()
        .filter(|d| d.source == drift::CorpusSource::Pg)
        .collect();
    eprintln!("\nPg corpus drift report ({} entries):", pg.len());
    let by_kind = |kind: drift::DisagreementKind| -> Vec<&&drift::Disagreement> {
        pg.iter().filter(|d| d.kind == kind).collect()
    };
    let lint_flags = by_kind(drift::DisagreementKind::LintFlagsGrammarAccepts);
    let lint_quiet = by_kind(drift::DisagreementKind::GrammarRejectsLintQuiet);
    eprintln!(
        "  LintFlagsGrammarAccepts: {} (dsql-lint over-flags / grammar relaxed)",
        lint_flags.len()
    );
    for d in &lint_flags {
        eprintln!("    {}", d.sql);
    }
    eprintln!(
        "  GrammarRejectsLintQuiet: {} (potential MissingDsqlLintRule)",
        lint_quiet.len()
    );
    for d in &lint_quiet {
        eprintln!("    {}", d.sql);
    }
}
