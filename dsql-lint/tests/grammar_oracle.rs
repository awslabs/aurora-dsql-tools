//! Grammar oracle: test-time recognizer for `dsql_grammar.ebnf`.
//!
//! Loads the upstream EBNF, builds a chumsky recognizer, and asserts
//! dsql-lint and the recognizer agree on every test case. Disagreements
//! tracked in an `EXPECTED_DRIFT` const (see Phase 4); the list shrinks
//! over time as rules are added.

#[path = "grammar_oracle/mod.rs"]
mod grammar_oracle;
