//! Helpers for the grammar oracle integration test.
//!
//! Submodules:
//! - `snowglobe`: load `dsql_grammar.json` (Snowglobe-shaped grammar);
//!   input-driven derivation oracle answering "does the grammar accept
//!   this SQL?"
//! - `pg_corpus`: load vendored Postgres regression-test SQL.
//! - `drift`: corpus + expected-drift list used by the agreement test.

pub mod drift;
pub mod pg_corpus;
pub mod snowglobe;
