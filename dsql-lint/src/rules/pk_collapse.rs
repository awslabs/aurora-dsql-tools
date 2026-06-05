//! Detection AND collapse of standalone `ALTER TABLE ... ADD CONSTRAINT
//! ... PRIMARY KEY (...)` statements into inline PRIMARY KEY constraints
//! on the preceding `CREATE TABLE`.
//!
//! ## Why collapse?
//! PostgreSQL stores PRIMARY KEY constraints separately from the table
//! definition, so `pg_dump` always normalizes them out into:
//!
//! ```sql
//! CREATE TABLE public.users (
//!     id integer NOT NULL,
//!     email text NOT NULL
//! );
//! ALTER TABLE ONLY public.users
//!     ADD CONSTRAINT users_pkey PRIMARY KEY (id);
//! ```
//!
//! DSQL accepts PRIMARY KEY only when defined inline on the `CREATE
//! TABLE` itself; standalone `ALTER TABLE ... ADD CONSTRAINT ... PRIMARY
//! KEY` is rejected. The DSQL-idiomatic shape is:
//!
//! ```sql
//! CREATE TABLE public.users (
//!     id integer NOT NULL,
//!     email text NOT NULL,
//!     CONSTRAINT users_pkey PRIMARY KEY (id)
//! );
//! ```
//!
//! This module folds the ALTER's `PrimaryKeyConstraint` straight onto
//! the CREATE TABLE's `constraints` vector — no field reshuffling, no
//! mutation of the column list — so the output `Display` carries the
//! same constraint name, columns, and characteristics. The redundant
//! `ALTER TABLE` part is then removed.
//!
//! ## When the collapse applies
//! All of the following must hold:
//! - There is a parseable `CREATE TABLE T` in the input.
//! - There is an `ALTER TABLE T` whose operations are EXACTLY one
//!   `AddConstraint { constraint: TableConstraint::PrimaryKey(...) }`. A
//!   multi-op ALTER (e.g. `ADD CONSTRAINT a PRIMARY KEY (...), ADD
//!   CONSTRAINT b UNIQUE (...)`) is left to the per-statement rule,
//!   which still surfaces it as Unfixable.
//! - The ALTER comes *after* the CREATE TABLE in source order. (If a
//!   future caller re-orders statements before linting, an ALTER before
//!   its CREATE TABLE would not fold — the per-statement rule handles
//!   it.)
//! - The constraint variant is plain `PrimaryKey(_)`. The PG-specific
//!   `PrimaryKeyUsingIndex` variant (created by `ALTER TABLE ... ADD
//!   CONSTRAINT ... PRIMARY KEY USING INDEX ...`) is skipped — it
//!   carries different semantics and DSQL has its own rule for it
//!   (`AtUnsupportedPrimaryKeyUsingIndex`).
//!
//! Cross-file ALTER (CREATE TABLE not present in the input) is left to
//! the per-statement rule so the user is told to bring the CREATE TABLE
//! into scope or rewrite the dump.

use sqlparser::ast::{AlterTableOperation, PrimaryKeyConstraint, Statement, TableConstraint};
use sqlparser::dialect::PostgreSqlDialect;
use sqlparser::parser::Parser;

use crate::lint::{Diagnostic, FixResult, LintRule};
use crate::rules::name_match::{
    drop_parts, normalize_object_name, parse_parts, pick_best_match, refs_match, NameRef,
};

/// A fold-able `ALTER TABLE ... ADD CONSTRAINT ... PRIMARY KEY (...)`.
/// Carries indices into the *parsed* statement slice (NOT into the raw
/// `parts` list); the lint/fix passes translate via `parsed_to_part`.
#[derive(Debug, Clone)]
pub(crate) struct PrimaryKeyAddIdiom {
    /// Normalized ref of the table being altered.
    pub table: NameRef,
    /// The full PRIMARY KEY constraint to fold onto the CREATE TABLE.
    pub constraint: PrimaryKeyConstraint,
    /// Index of the `CREATE TABLE` in the parsed slice.
    pub create_table_index: usize,
    /// Index of the redundant `ALTER TABLE` in the parsed slice.
    pub alter_index: usize,
}

/// Detect every fold-able standalone `ALTER TABLE ... ADD CONSTRAINT
/// ... PRIMARY KEY (...)` in `stmts`. Each result names a CREATE TABLE
/// that is already present in `stmts` (otherwise the fold has no target
/// — the per-statement rule handles that case as Unfixable).
pub(crate) fn detect_alter_add_primary_key(stmts: &[Statement]) -> Vec<PrimaryKeyAddIdiom> {
    // Build a side index of CREATE TABLE positions so we can correlate
    // each ALTER with its target without an O(N^2) scan.
    let creates: Vec<(NameRef, usize)> = stmts
        .iter()
        .enumerate()
        .filter_map(|(idx, stmt)| {
            if let Statement::CreateTable(ct) = stmt {
                normalize_object_name(&ct.name).map(|n| (n, idx))
            } else {
                None
            }
        })
        .collect();

    let mut idioms = Vec::new();
    for (alter_index, stmt) in stmts.iter().enumerate() {
        let Statement::AlterTable(alter) = stmt else {
            continue;
        };
        // Single-op ALTER only; a multi-op ALTER is more complex and
        // safer to leave to the per-statement Unfixable path.
        let [op] = alter.operations.as_slice() else {
            continue;
        };
        let AlterTableOperation::AddConstraint { constraint, .. } = op else {
            continue;
        };
        let TableConstraint::PrimaryKey(pk) = constraint else {
            // `PrimaryKeyUsingIndex` is a different shape; skip it.
            continue;
        };
        let Some(table_ref) = normalize_object_name(&alter.name) else {
            continue;
        };

        // Find the CREATE TABLE for this table — and require it to
        // appear BEFORE the ALTER in source order (an ALTER before its
        // CREATE TABLE is malformed, even if technically in the slice).
        // `pick_best_match` prefers an exact-schema match over a wildcard
        // one, so a qualified ALTER does not steal an unqualified CREATE
        // that happened to appear first.
        let Some((_, create_idx)) = pick_best_match(
            &creates,
            &table_ref,
            |(n, _)| n,
            |(_, idx)| *idx < alter_index,
        ) else {
            continue;
        };

        idioms.push(PrimaryKeyAddIdiom {
            table: table_ref,
            constraint: pk.clone(),
            create_table_index: *create_idx,
            alter_index,
        });
    }
    idioms
}

/// Lint-mode pass: surface a `AlterAddPrimaryKeyCollapse` diagnostic per
/// fold-able idiom so users running `--lint` (no `--fix`) see what
/// `--fix` would do. Runs BEFORE the per-statement loop in `lint_sql`;
/// the per-statement `AtUnsupportedAddPrimaryKey` rule still fires in
/// lint mode (suppression only happens in fix mode, where the ALTER is
/// removed from `parts` before the per-statement loop sees it).
pub(crate) fn check_alter_add_primary_key(
    parts: &[(usize, String)],
    diagnostics: &mut Vec<Diagnostic>,
) {
    let (parsed, parsed_to_part) = parse_parts(parts);
    for idiom in detect_alter_add_primary_key(&parsed) {
        let alter_part = parsed_to_part[idiom.alter_index];
        diagnostics.push(Diagnostic {
            rule: LintRule::AlterAddPrimaryKeyCollapse,
            line: parts[alter_part].0,
            statement: parts[alter_part].1.clone(),
            message: format!(
                "ALTER TABLE ADD CONSTRAINT ... PRIMARY KEY on `{table}` is not supported in DSQL. \
                 Define PRIMARY KEY inline on the CREATE TABLE.",
                table = idiom.table.1,
            ),
            suggestion: format!(
                "Move the PRIMARY KEY constraint into the `CREATE TABLE {table}` definition.",
                table = idiom.table.1,
            ),
            fix_result: FixResult::Fixed(format!(
                "Folded `{}` PRIMARY KEY constraint into the CREATE TABLE definition.",
                idiom.table.1,
            )),
        });
    }
}

/// Fix-mode pass: rewrite each idiom's CREATE TABLE to embed the PRIMARY
/// KEY constraint inline, then drop the redundant ALTER TABLE part. Runs
/// BEFORE the per-statement loop so the per-statement
/// `AtUnsupportedAddPrimaryKey` rule never sees these statements (the
/// parts are gone by then).
pub(crate) fn fix_alter_add_primary_key(
    parts: &mut Vec<(usize, String)>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let dialect = PostgreSqlDialect {};

    let (parsed, parsed_to_part) = parse_parts(parts);
    let idioms = detect_alter_add_primary_key(&parsed);
    if idioms.is_empty() {
        return;
    }

    let mut parts_to_remove: Vec<usize> = Vec::new();

    for idiom in &idioms {
        let create_table_part = parsed_to_part[idiom.create_table_index];
        let alter_part = parsed_to_part[idiom.alter_index];

        // Re-parse the CREATE TABLE part (rather than mutate the shared
        // parsed copy) so we emit exactly that statement's canonical
        // text. Both arms below should be unreachable — `parse_parts`
        // already proved this text parses, and `detect_alter_add_primary_key`
        // already proved the CREATE TABLE matches the idiom's table. If
        // they ever fire, the silent `continue` would mask a real
        // regression (constraint dropped from output, ALTER kept and
        // flagged Unfixable by the per-statement rule), so guard with
        // `debug_assert!` so test runs catch the drift loudly. Mirrors
        // the safety guards in `unique_collapse::fix_alter_add_unique`.
        let Ok(mut stmts) = Parser::parse_sql(&dialect, parts[create_table_part].1.trim()) else {
            debug_assert!(
                false,
                "re-parse of CREATE TABLE for `{}` failed after parse_parts succeeded",
                idiom.table.1
            );
            continue;
        };
        let mut folded = false;
        for stmt in &mut stmts {
            if let Statement::CreateTable(ct) = stmt {
                if normalize_object_name(&ct.name).is_some_and(|n| refs_match(&n, &idiom.table)) {
                    ct.constraints
                        .push(TableConstraint::PrimaryKey(idiom.constraint.clone()));
                    folded = true;
                    break;
                }
            }
        }
        if !folded {
            debug_assert!(
                false,
                "CREATE TABLE for `{}` not found after detect_alter_add_primary_key confirmed it",
                idiom.table.1
            );
            continue;
        }
        parts[create_table_part].1 = stmts
            .iter()
            .map(|s| format!("{s:#}"))
            .collect::<Vec<_>>()
            .join(";\n");

        parts_to_remove.push(alter_part);

        diagnostics.push(Diagnostic {
            rule: LintRule::AlterAddPrimaryKeyCollapse,
            line: parts[alter_part].0,
            statement: parts[alter_part].1.clone(),
            message: format!(
                "ALTER TABLE ADD CONSTRAINT ... PRIMARY KEY on `{table}` is not supported in DSQL. \
                 Define PRIMARY KEY inline on the CREATE TABLE.",
                table = idiom.table.1,
            ),
            suggestion: format!(
                "Moved the PRIMARY KEY constraint into the `CREATE TABLE {table}` definition.",
                table = idiom.table.1,
            ),
            fix_result: FixResult::Fixed(format!(
                "Folded `{}` PRIMARY KEY constraint into the CREATE TABLE definition.",
                idiom.table.1,
            )),
        });
    }

    drop_parts(parts, parts_to_remove);
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlparser::dialect::PostgreSqlDialect;
    use sqlparser::parser::Parser;

    fn parse_ok(stmts: &[&str]) -> Vec<Statement> {
        let dialect = PostgreSqlDialect {};
        stmts
            .iter()
            .filter_map(|s| Parser::parse_sql(&dialect, s).ok())
            .flatten()
            .collect()
    }

    #[test]
    fn detects_simple_add_primary_key_after_create_table() {
        let stmts = parse_ok(&[
            "CREATE TABLE public.users (id integer NOT NULL, email text NOT NULL)",
            "ALTER TABLE ONLY public.users ADD CONSTRAINT users_pkey PRIMARY KEY (id)",
        ]);
        let idioms = detect_alter_add_primary_key(&stmts);
        assert_eq!(idioms.len(), 1, "got: {idioms:?}");
        assert_eq!(idioms[0].table, (Some("public".into()), "users".into()));
        assert_eq!(idioms[0].create_table_index, 0);
        assert_eq!(idioms[0].alter_index, 1);
    }

    #[test]
    fn alter_before_create_table_is_not_a_match() {
        // ALTER comes BEFORE the CREATE TABLE — leave for the per-
        // statement rule to flag rather than fold (the resulting CREATE
        // TABLE would carry the constraint but the input was malformed).
        let stmts = parse_ok(&[
            "ALTER TABLE ONLY public.users ADD CONSTRAINT pk PRIMARY KEY (id)",
            "CREATE TABLE public.users (id integer NOT NULL, email text NOT NULL)",
        ]);
        let idioms = detect_alter_add_primary_key(&stmts);
        assert!(
            idioms.is_empty(),
            "expected no folds when ALTER precedes CREATE: {idioms:?}"
        );
    }

    #[test]
    fn unrelated_table_is_not_folded_into_wrong_create() {
        // ALTER on `orders` must NOT fold into `CREATE TABLE users`.
        let stmts = parse_ok(&[
            "CREATE TABLE public.users (id integer NOT NULL)",
            "ALTER TABLE ONLY public.orders ADD CONSTRAINT pk PRIMARY KEY (oid)",
        ]);
        let idioms = detect_alter_add_primary_key(&stmts);
        assert!(idioms.is_empty(), "got: {idioms:?}");
    }

    #[test]
    fn cross_file_alter_without_create_is_skipped() {
        // No CREATE TABLE in the input → leave for the per-statement
        // Unfixable rule.
        let stmts = parse_ok(&["ALTER TABLE ONLY public.users ADD CONSTRAINT pk PRIMARY KEY (id)"]);
        let idioms = detect_alter_add_primary_key(&stmts);
        assert!(idioms.is_empty());
    }

    #[test]
    fn unqualified_alter_matches_qualified_create() {
        // pg_dump reliably emits `public.t` everywhere, but we tolerate
        // mixed schema-qualification per the wildcard rule documented
        // on `refs_match`.
        let stmts = parse_ok(&[
            "CREATE TABLE public.users (id integer NOT NULL)",
            "ALTER TABLE ONLY users ADD CONSTRAINT pk PRIMARY KEY (id)",
        ]);
        let idioms = detect_alter_add_primary_key(&stmts);
        assert_eq!(idioms.len(), 1);
    }

    #[test]
    fn case_folded_unquoted_names_match() {
        // `My_Table` (unquoted) folds to `my_table` per PG; the ALTER's
        // bare `MY_TABLE` should still match.
        let stmts = parse_ok(&[
            "CREATE TABLE My_Table (id integer NOT NULL)",
            "ALTER TABLE ONLY MY_TABLE ADD CONSTRAINT pk PRIMARY KEY (id)",
        ]);
        let idioms = detect_alter_add_primary_key(&stmts);
        assert_eq!(idioms.len(), 1);
    }

    #[test]
    fn quoted_mixed_case_names_must_match_exactly() {
        // Quoted identifiers preserve case; "My_Table" (quoted) does
        // NOT equal `My_Table` (unquoted, folds to `my_table`).
        let stmts = parse_ok(&[
            "CREATE TABLE \"My_Table\" (id integer NOT NULL)",
            "ALTER TABLE ONLY \"my_table\" ADD CONSTRAINT pk PRIMARY KEY (id)",
        ]);
        let idioms = detect_alter_add_primary_key(&stmts);
        assert!(
            idioms.is_empty(),
            "case-sensitive mismatch on quoted name should NOT fold: {idioms:?}"
        );
    }

    #[test]
    fn multi_op_alter_is_left_alone() {
        // Multi-op ALTER (uncommon but legal): leave to per-statement
        // rule so the user gets a clear Unfixable rather than a partial
        // fold.
        let stmts = parse_ok(&[
            "CREATE TABLE public.t (a integer, b integer)",
            "ALTER TABLE ONLY public.t \
                ADD CONSTRAINT pk PRIMARY KEY (a), \
                ADD CONSTRAINT u UNIQUE (b)",
        ]);
        let idioms = detect_alter_add_primary_key(&stmts);
        assert!(
            idioms.is_empty(),
            "multi-op ALTER must not fold: {idioms:?}"
        );
    }

    #[test]
    fn exact_schema_match_preferred_over_wildcard() {
        // When both an unqualified `t` and a schema-qualified `public.t`
        // exist, a qualified ALTER must correlate with the qualified CREATE
        // — not the wildcard match that happens to appear earlier in
        // source order.
        let stmts = parse_ok(&[
            "CREATE TABLE t (id integer)",
            "CREATE TABLE public.t (id integer)",
            "ALTER TABLE ONLY public.t ADD CONSTRAINT pk PRIMARY KEY (id)",
        ]);
        let idioms = detect_alter_add_primary_key(&stmts);
        assert_eq!(idioms.len(), 1, "expected 1 idiom: {idioms:?}");
        // create_table_index 1 is `public.t`, NOT 0 (the unqualified `t`).
        assert_eq!(idioms[0].create_table_index, 1);
        assert_eq!(idioms[0].table, (Some("public".into()), "t".into()));
    }

    #[test]
    fn include_columns_round_trip() {
        // pg_dump can emit `INCLUDE (col)` covering columns on a PK constraint.
        // The fold must carry the INCLUDE through verbatim — DSQL accepts the
        // inline form, and dropping the clause would silently change the
        // declared schema.
        let stmts = parse_ok(&[
            "CREATE TABLE t (id integer NOT NULL, payload text)",
            "ALTER TABLE ONLY t ADD CONSTRAINT t_pk PRIMARY KEY (id) INCLUDE (payload)",
        ]);
        let idioms = detect_alter_add_primary_key(&stmts);
        assert_eq!(idioms.len(), 1, "got: {idioms:?}");
        let include: Vec<String> = idioms[0]
            .constraint
            .include
            .iter()
            .map(|i| i.value.clone())
            .collect();
        assert_eq!(include, vec!["payload".to_string()]);
    }
}
