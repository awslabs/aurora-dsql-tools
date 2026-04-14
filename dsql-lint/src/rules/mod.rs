use crate::lint::Diagnostic;
use sqlparser::ast::Statement;

pub mod errors;
pub mod warnings;

/// Find the 1-based line number of `needle` (case-insensitive) for diagnostic reporting.
pub(crate) fn find_line(raw_sql: &str, needle: &str) -> usize {
    let lower = raw_sql.to_lowercase();
    if let Some(pos) = lower.find(needle) {
        raw_sql[..pos].matches('\n').count() + 1
    } else {
        1
    }
}

pub fn check_statement(stmt: &Statement, raw_sql: &str, diagnostics: &mut Vec<Diagnostic>) {
    errors::check(stmt, raw_sql, diagnostics);
    warnings::check(stmt, raw_sql, diagnostics);
}
