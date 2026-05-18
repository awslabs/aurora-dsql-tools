//! Helpers for the grammar oracle integration test.
//!
//! Submodules:
//! - `grammar`: load `dsql_grammar.json`; input-driven derivation
//!   oracle answering "does the grammar accept this SQL?"
//! - `pg_corpus`: load vendored Postgres regression-test SQL.
//! - `drift`: corpus + expected-drift list used by the agreement test.

pub mod drift;
pub mod grammar;
pub mod pg_corpus;
