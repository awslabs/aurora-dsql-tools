use crate::lint::Diagnostic;
use sqlparser::ast::Statement;

pub mod errors;

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
            || (!lower.as_bytes()[abs_pos - 1].is_ascii_alphanumeric()
                && lower.as_bytes()[abs_pos - 1] != b'_');
        let after_ok = end_pos >= lower.len()
            || (!lower.as_bytes()[end_pos].is_ascii_alphanumeric()
                && lower.as_bytes()[end_pos] != b'_');

        if before_ok && after_ok {
            return Some(raw_sql[..abs_pos].matches('\n').count() + 1);
        }
        start = abs_pos + 1;
    }
    None
}

/// Find the 1-based line number of `needle` (case-insensitive, word-boundary-aware).
/// Returns 1 if `needle` is not found.
pub(crate) fn find_line(raw_sql: &str, needle: &str) -> usize {
    try_find_line(raw_sql, needle).unwrap_or(1)
}

/// Try each needle in order, return the line of the first match.
/// Returns 1 if none of the needles are found.
pub(crate) fn find_line_any(raw_sql: &str, needles: &[&str]) -> usize {
    for needle in needles {
        if let Some(line) = try_find_line(raw_sql, needle) {
            return line;
        }
    }
    1
}

pub(crate) fn check_statement(
    stmt: &mut Statement,
    raw_sql: &str,
    diagnostics: &mut Vec<Diagnostic>,
) {
    errors::check(stmt, raw_sql, diagnostics);
}
