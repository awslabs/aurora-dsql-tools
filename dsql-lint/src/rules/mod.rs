//! DSQL compatibility rules applied to parsed SQL AST.

use sqlparser::ast::Statement;
use crate::lint::Diagnostic;

pub mod errors;
pub mod warnings;

/// Run all rules against a single statement.
pub fn check_statement(stmt: &Statement, raw_sql: &str, diagnostics: &mut Vec<Diagnostic>) {
    errors::check(stmt, raw_sql, diagnostics);
    warnings::check(stmt, diagnostics);
}
