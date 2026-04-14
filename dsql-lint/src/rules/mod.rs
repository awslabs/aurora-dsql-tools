use crate::lint::Diagnostic;
use sqlparser::ast::Statement;

pub mod errors;
pub mod warnings;

/// Find the 1-based line number of `needle` (case-insensitive, word-boundary-aware).
/// Returns `None` when no word-bounded match exists.
fn try_find_line(raw_sql: &str, needle: &str) -> Option<usize> {
    let lower = raw_sql.to_lowercase();
    let needle_lower = needle.to_lowercase();

    let mut start = 0;
    while let Some(pos) = lower[start..].find(&needle_lower) {
        let abs_pos = start + pos;
        let end_pos = abs_pos + needle_lower.len();

        let before_ok = abs_pos == 0
            || !raw_sql.as_bytes()[abs_pos - 1]
                .is_ascii_alphanumeric()
                && raw_sql.as_bytes()[abs_pos - 1] != b'_';
        let after_ok = end_pos >= raw_sql.len()
            || !raw_sql.as_bytes()[end_pos].is_ascii_alphanumeric()
                && raw_sql.as_bytes()[end_pos] != b'_';

        if before_ok && after_ok {
            return Some(raw_sql[..abs_pos].matches('\n').count() + 1);
        }
        start = abs_pos + 1;
    }
    None
}

/// Find the 1-based line number of `needle` (case-insensitive, word-boundary-aware).
///
/// Panics if `needle` is not found — callers only search for keywords the AST
/// already confirmed exist, so a missing match indicates a bug.
pub(crate) fn find_line(raw_sql: &str, needle: &str) -> usize {
    try_find_line(raw_sql, needle)
        .unwrap_or_else(|| panic!("BUG: find_line could not find `{needle}` in SQL"))
}

/// Try each needle in order, return the line of the first match.
/// Panics if none of the needles are found.
pub(crate) fn find_line_any(raw_sql: &str, needles: &[&str]) -> usize {
    for needle in needles {
        if let Some(line) = try_find_line(raw_sql, needle) {
            return line;
        }
    }
    panic!("BUG: find_line_any could not find any of {needles:?} in SQL")
}

pub fn check_statement(stmt: &Statement, raw_sql: &str, diagnostics: &mut Vec<Diagnostic>) {
    errors::check(stmt, raw_sql, diagnostics);
    warnings::check(stmt, raw_sql, diagnostics);
}
