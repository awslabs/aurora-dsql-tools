//! Shared helpers for whole-list rules that correlate statements by table /
//! sequence name (`serial_idiom`, `constraint_collapse`).
//!
//! Both rules need the same primitives:
//! - PG case-folded identifier comparison (`fold_ident`)
//! - `ObjectName` → `(Option<schema>, name)` normalization (`normalize_object_name`)
//! - schema-wildcard match with exact-match preference (`refs_match` + `pick_best_match`)
//! - per-`parts` re-parse with origin tracking (`parse_parts`)
//! - removal of folded-away parts (`drop_parts`)

use sqlparser::ast::{Ident, ObjectName, Statement};
use sqlparser::dialect::PostgreSqlDialect;
use sqlparser::parser::Parser;

/// A normalized `(optional schema, name)` reference, PG case-folded so byte-
/// equal `==` respects PostgreSQL's case rules.
pub(crate) type NameRef = (Option<String>, String);

/// PG case-folding for an `Ident` parsed by sqlparser. Unquoted identifiers
/// fold to lowercase; quoted identifiers keep their case verbatim. Mirrors
/// what the PostgreSQL server does at parse time.
pub(crate) fn fold_ident(ident: &Ident) -> String {
    if ident.quote_style.is_some() {
        ident.value.clone()
    } else {
        ident.value.to_ascii_lowercase()
    }
}

/// Normalize an `ObjectName` to `(Option<schema>, name)` with PG case folding.
///
/// - 0 parts -> `None`
/// - 1 part  -> `(None, name)`
/// - 2+ parts -> the trailing pair is taken as `(schema, name)` (handles
///   `db.schema.table` by keeping the trailing schema-qualified pair).
pub(crate) fn normalize_object_name(name: &ObjectName) -> Option<NameRef> {
    let folded: Vec<String> = name
        .0
        .iter()
        .filter_map(|part| part.as_ident())
        .map(fold_ident)
        .collect();
    match folded.as_slice() {
        [] => None,
        [n] => Some((None, n.clone())),
        [.., schema, n] => Some((Some(schema.clone()), n.clone())),
    }
}

/// Two normalized refs match if names are equal AND schemas agree where both
/// are present. A missing schema on either side is a wildcard (pg_dump may
/// emit `public.t` in one statement and bare `t` in another).
pub(crate) fn refs_match(a: &NameRef, b: &NameRef) -> bool {
    if a.1 != b.1 {
        return false;
    }
    match (&a.0, &b.0) {
        (Some(s1), Some(s2)) => s1 == s2,
        _ => true,
    }
}

/// Find the best `refs_match`-compatible candidate for `target` in `items`,
/// preferring an exact-schema match (both sides present and equal) over a
/// wildcard match (one side missing a schema). `key` projects each candidate
/// to its `NameRef`; `extra` is an additional predicate the candidate must
/// also satisfy (e.g. "the table's columns include the one we're wiring",
/// "this CREATE TABLE appears before the ALTER").
///
/// Why preference matters: if both `CREATE TABLE t` and `CREATE TABLE
/// public.t` exist, an `ALTER TABLE ONLY public.t` must correlate with the
/// qualified one — not a wildcard hit on the unqualified `t` that happened
/// to appear first.
pub(crate) fn pick_best_match<'a, T>(
    items: &'a [T],
    target: &NameRef,
    key: impl Fn(&'a T) -> &'a NameRef,
    extra: impl Fn(&'a T) -> bool,
) -> Option<&'a T> {
    let mut wildcard: Option<&'a T> = None;
    for item in items {
        let candidate = key(item);
        if !refs_match(candidate, target) || !extra(item) {
            continue;
        }
        if candidate.0.is_some() && target.0.is_some() {
            return Some(item);
        }
        if wildcard.is_none() {
            wildcard = Some(item);
        }
    }
    wildcard
}

/// Re-parse each part into statements, tracking which part each statement
/// came from. Statements from un-parseable parts are dropped silently — the
/// per-statement loop in `lint_sql` / `fix_sql` reports those as `ParseError`
/// against the original text, so swallowing here is correct.
pub(crate) fn parse_parts(parts: &[(usize, String)]) -> (Vec<Statement>, Vec<usize>) {
    let dialect = PostgreSqlDialect {};
    let mut parsed: Vec<Statement> = Vec::new();
    let mut parsed_to_part: Vec<usize> = Vec::new();
    for (part_idx, (_, text)) in parts.iter().enumerate() {
        if let Ok(stmts) = Parser::parse_sql(&dialect, text.trim()) {
            for stmt in stmts {
                parsed.push(stmt);
                parsed_to_part.push(part_idx);
            }
        }
    }
    (parsed, parsed_to_part)
}

/// Drop `indices` from `parts`. Sorts and dedups first, then removes in
/// reverse so earlier removals don't shift later indices. Used by every
/// whole-list fix pass to drop folded-away ALTER statements once their
/// constraint has been moved onto the CREATE TABLE.
pub(crate) fn drop_parts(parts: &mut Vec<(usize, String)>, mut indices: Vec<usize>) {
    indices.sort_unstable();
    indices.dedup();
    for idx in indices.into_iter().rev() {
        parts.remove(idx);
    }
}

/// Parse each input string as PG SQL and collect the statements that
/// parse, dropping the rest. Shared between rule-module test suites
/// that exercise multi-statement detection.
#[cfg(test)]
pub(crate) fn parse_ok(stmts: &[&str]) -> Vec<Statement> {
    let dialect = PostgreSqlDialect {};
    stmts
        .iter()
        .filter_map(|s| Parser::parse_sql(&dialect, s).ok())
        .flatten()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drop_parts_sorts_dedups_and_removes_in_reverse() {
        let mut parts = vec![(1, "a".into()), (2, "b".into()), (3, "c".into())];
        drop_parts(&mut parts, vec![2, 0, 0]);
        assert_eq!(parts, vec![(2, "b".into())]);
    }
}
