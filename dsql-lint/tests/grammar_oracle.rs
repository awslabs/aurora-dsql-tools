//! Grammar oracle: test-time recognizer for `dsql_grammar.ebnf`.
//!
//! Loads the upstream EBNF, builds a chumsky recognizer, and asserts that
//! dsql-lint and the recognizer agree on the test corpus. Drift is tracked
//! in `KNOWN_DRIFT.md` with the goal of driving the list to zero.

#[path = "grammar_oracle/mod.rs"]
mod grammar_oracle;
