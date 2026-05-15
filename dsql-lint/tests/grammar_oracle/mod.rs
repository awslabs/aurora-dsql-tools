//! Helpers for the grammar oracle integration test.
//!
//! Submodules:
//! - `ebnf`: parse `dsql_grammar.ebnf` text into a typed `Grammar` AST.
//! - `recognizer`: build a chumsky recognizer from the `Grammar` AST.
//! - `drift`: corpus + expected-drift list used by the agreement test.

pub mod drift;
pub mod ebnf;
pub mod recognizer;
