use crate::lint::Diagnostic;
use sqlparser::ast::Statement;

pub mod errors;
pub mod warnings;

pub fn check_statement(stmt: &Statement, raw_sql: &str, diagnostics: &mut Vec<Diagnostic>) {
    errors::check(stmt, raw_sql, diagnostics);
    warnings::check(stmt, diagnostics);
}
