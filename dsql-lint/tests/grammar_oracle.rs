//! Grammar oracle: test-time derivation oracle for `dsql_grammar.json`.
//!
//! Loads the vendored grammar JSON and exposes `accepts(sql) -> bool`.
//! Subsequent commits add drift detection against the dsql-lint test
//! corpus and the upstream Postgres regression-test corpus.

#[path = "grammar_oracle/mod.rs"]
mod grammar_oracle;
