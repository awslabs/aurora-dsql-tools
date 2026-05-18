//! Helpers for the grammar oracle integration test.
//!
//! Submodules:
//! - `ebnf`: parse `dsql_grammar.ebnf` text into a typed `Grammar` AST.
//! - `recognizer`: build a chumsky recognizer from the `Grammar` AST.
//! - `snowglobe`: load `dsql_grammar.json` (Snowglobe shape); reachability oracle.
//! - `pg_corpus`: load vendored Postgres regression-test SQL.
//! - `drift`: corpus + expected-drift list used by the agreement test.

pub mod drift;
pub mod ebnf;
pub mod pg_corpus;
pub mod recognizer;
pub mod snowglobe;
