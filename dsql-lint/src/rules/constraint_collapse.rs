//! Detection AND collapse of standalone `ALTER TABLE ... ADD CONSTRAINT
//! ... [PRIMARY KEY|UNIQUE] (...)` statements into inline constraints
//! on the preceding `CREATE TABLE`.
//!
//! pg_dump emits PRIMARY KEY and UNIQUE constraints as separate ALTER
//! TABLE statements after the CREATE TABLE, but DSQL only accepts them
//! inline. The two rules share one fold engine here — only the
//! constraint AST type and rule name differ, captured by [`Kind<C>`].

use sqlparser::ast::{
    AlterTableOperation, PrimaryKeyConstraint, Statement, TableConstraint, UniqueConstraint,
};
use sqlparser::dialect::PostgreSqlDialect;
use sqlparser::parser::Parser;

use crate::lint::{Diagnostic, FixResult, LintRule};
use crate::rules::name_match::{
    drop_parts, normalize_object_name, parse_parts, pick_best_match, refs_match, NameRef,
};

/// Per-rule configuration: the LintRule variant, the user-facing
/// keyword, and how to extract / wrap the constraint AST node.
struct Kind<C> {
    rule: LintRule,
    keyword: &'static str,
    extract: fn(&TableConstraint) -> Option<&C>,
    wrap: fn(C) -> TableConstraint,
}

const PK_KIND: Kind<PrimaryKeyConstraint> = Kind {
    rule: LintRule::AlterAddPrimaryKeyCollapse,
    keyword: "PRIMARY KEY",
    extract: |c| match c {
        TableConstraint::PrimaryKey(pk) => Some(pk),
        _ => None,
    },
    wrap: TableConstraint::PrimaryKey,
};

const UNIQUE_KIND: Kind<UniqueConstraint> = Kind {
    rule: LintRule::AlterAddUniqueCollapse,
    keyword: "UNIQUE",
    extract: |c| match c {
        TableConstraint::Unique(u) => Some(u),
        _ => None,
    },
    wrap: TableConstraint::Unique,
};

/// A fold-able idiom: an `ALTER TABLE ... ADD CONSTRAINT ...` whose
/// target CREATE TABLE is present earlier in the same input. Indices
/// point into the *parsed* statement slice, not the raw `parts` list;
/// callers translate via `parse_parts`'s `parsed_to_part` mapping.
struct Idiom<C> {
    table: NameRef,
    constraint: C,
    create_table_index: usize,
    alter_index: usize,
}

/// Find every fold-able single-op `ALTER TABLE ... ADD CONSTRAINT ...`
/// of the kind selected by `kind` whose target CREATE TABLE appears
/// earlier in `stmts`. Multi-op ALTERs and the `*UsingIndex` variants
/// are left for the per-statement Unfixable path.
fn detect<C: Clone>(stmts: &[Statement], kind: &Kind<C>) -> Vec<Idiom<C>> {
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

    let mut idioms = Vec::with_capacity(stmts.len());
    for (alter_index, stmt) in stmts.iter().enumerate() {
        let Statement::AlterTable(alter) = stmt else {
            continue;
        };
        let [op] = alter.operations.as_slice() else {
            continue;
        };
        let AlterTableOperation::AddConstraint { constraint, .. } = op else {
            continue;
        };
        let Some(c) = (kind.extract)(constraint) else {
            continue;
        };
        let Some(table_ref) = normalize_object_name(&alter.name) else {
            continue;
        };

        // `pick_best_match` prefers exact-schema over wildcard, so a
        // qualified ALTER does not steal an unqualified CREATE that
        // happened to appear first.
        let Some((_, create_idx)) = pick_best_match(
            &creates,
            &table_ref,
            |(n, _)| n,
            |(_, idx)| *idx < alter_index,
        ) else {
            continue;
        };

        idioms.push(Idiom {
            table: table_ref,
            constraint: c.clone(),
            create_table_index: *create_idx,
            alter_index,
        });
    }
    idioms
}

fn diag<C>(
    idiom: &Idiom<C>,
    line: usize,
    statement: String,
    kind: &Kind<C>,
    moved: bool,
) -> Diagnostic {
    let verb = if moved { "Moved" } else { "Move" };
    let kw = kind.keyword;
    let table = &idiom.table.1;
    Diagnostic {
        rule: kind.rule,
        line,
        statement,
        message: format!(
            "ALTER TABLE ADD CONSTRAINT ... {kw} on `{table}` is not supported in DSQL. \
             Define {kw} inline on the CREATE TABLE."
        ),
        suggestion: format!(
            "{verb} the {kw} constraint into the `CREATE TABLE {table}` definition."
        ),
        fix_result: FixResult::Fixed(format!(
            "Folded `{table}` {kw} constraint into the CREATE TABLE definition."
        )),
    }
}

fn check<C: Clone>(parts: &[(usize, String)], diagnostics: &mut Vec<Diagnostic>, kind: &Kind<C>) {
    let (parsed, parsed_to_part) = parse_parts(parts);
    for idiom in detect(&parsed, kind) {
        let alter_part = parsed_to_part[idiom.alter_index];
        diagnostics.push(diag(
            &idiom,
            parts[alter_part].0,
            parts[alter_part].1.clone(),
            kind,
            false,
        ));
    }
}

fn fix<C: Clone>(
    parts: &mut Vec<(usize, String)>,
    diagnostics: &mut Vec<Diagnostic>,
    kind: &Kind<C>,
) {
    let dialect = PostgreSqlDialect {};

    let (parsed, parsed_to_part) = parse_parts(parts);
    let idioms = detect(&parsed, kind);
    if idioms.is_empty() {
        return;
    }

    let mut parts_to_remove = Vec::with_capacity(idioms.len());

    for idiom in &idioms {
        let create_table_part = parsed_to_part[idiom.create_table_index];
        let alter_part = parsed_to_part[idiom.alter_index];

        // Re-parse rather than mutating the shared parsed copy so we
        // emit exactly that statement's canonical text. Both arms
        // below should be unreachable — `parse_parts` proved this text
        // parses, and `detect` proved the CREATE TABLE matches the
        // idiom's table. debug_assert! catches drift loudly in tests.
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
                    ct.constraints.push((kind.wrap)(idiom.constraint.clone()));
                    folded = true;
                    break;
                }
            }
        }
        if !folded {
            debug_assert!(
                false,
                "CREATE TABLE for `{}` not found after detect confirmed it",
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

        diagnostics.push(diag(
            idiom,
            parts[alter_part].0,
            parts[alter_part].1.clone(),
            kind,
            true,
        ));
    }

    drop_parts(parts, parts_to_remove);
}

// ── Per-rule public wrappers ────────────────────────────────────────

pub(crate) fn check_alter_add_primary_key(
    parts: &[(usize, String)],
    diagnostics: &mut Vec<Diagnostic>,
) {
    check(parts, diagnostics, &PK_KIND);
}

pub(crate) fn fix_alter_add_primary_key(
    parts: &mut Vec<(usize, String)>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    fix(parts, diagnostics, &PK_KIND);
}

pub(crate) fn check_alter_add_unique(parts: &[(usize, String)], diagnostics: &mut Vec<Diagnostic>) {
    check(parts, diagnostics, &UNIQUE_KIND);
}

pub(crate) fn fix_alter_add_unique(
    parts: &mut Vec<(usize, String)>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    fix(parts, diagnostics, &UNIQUE_KIND);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_ok(stmts: &[&str]) -> Vec<Statement> {
        let dialect = PostgreSqlDialect {};
        stmts
            .iter()
            .filter_map(|s| Parser::parse_sql(&dialect, s).ok())
            .flatten()
            .collect()
    }

    // Engine tests use PK as the canonical case; PK and UNIQUE share
    // the engine, and per-rule end-to-end coverage lives in
    // `tests/fix_test.rs` SNAPSHOT_CASES.

    #[test]
    fn detects_simple_add_after_create_table() {
        let stmts = parse_ok(&[
            "CREATE TABLE public.users (id integer NOT NULL, email text NOT NULL)",
            "ALTER TABLE ONLY public.users ADD CONSTRAINT users_pkey PRIMARY KEY (id)",
        ]);
        let idioms = detect(&stmts, &PK_KIND);
        assert_eq!(idioms.len(), 1);
        assert_eq!(idioms[0].table, (Some("public".into()), "users".into()));
        assert_eq!(idioms[0].create_table_index, 0);
        assert_eq!(idioms[0].alter_index, 1);
    }

    #[test]
    fn alter_before_create_table_does_not_match() {
        let stmts = parse_ok(&[
            "ALTER TABLE ONLY public.users ADD CONSTRAINT pk PRIMARY KEY (id)",
            "CREATE TABLE public.users (id integer NOT NULL)",
        ]);
        assert!(detect(&stmts, &PK_KIND).is_empty());
    }

    #[test]
    fn unrelated_table_is_not_folded() {
        let stmts = parse_ok(&[
            "CREATE TABLE public.users (id integer NOT NULL)",
            "ALTER TABLE ONLY public.orders ADD CONSTRAINT pk PRIMARY KEY (oid)",
        ]);
        assert!(detect(&stmts, &PK_KIND).is_empty());
    }

    #[test]
    fn cross_file_alter_without_create_is_skipped() {
        let stmts = parse_ok(&["ALTER TABLE ONLY public.users ADD CONSTRAINT pk PRIMARY KEY (id)"]);
        assert!(detect(&stmts, &PK_KIND).is_empty());
    }

    #[test]
    fn unqualified_alter_matches_qualified_create() {
        let stmts = parse_ok(&[
            "CREATE TABLE public.users (id integer NOT NULL)",
            "ALTER TABLE ONLY users ADD CONSTRAINT pk PRIMARY KEY (id)",
        ]);
        assert_eq!(detect(&stmts, &PK_KIND).len(), 1);
    }

    #[test]
    fn case_folded_unquoted_names_match() {
        // `My_Table` (unquoted) folds to `my_table` per PG; the bare
        // `MY_TABLE` ALTER must still match.
        let stmts = parse_ok(&[
            "CREATE TABLE My_Table (id integer NOT NULL)",
            "ALTER TABLE ONLY MY_TABLE ADD CONSTRAINT pk PRIMARY KEY (id)",
        ]);
        assert_eq!(detect(&stmts, &PK_KIND).len(), 1);
    }

    #[test]
    fn quoted_mixed_case_names_must_match_exactly() {
        // Quoted identifiers preserve case; "My_Table" (quoted) does
        // NOT equal `my_table` (unquoted, folds to `my_table`).
        let stmts = parse_ok(&[
            "CREATE TABLE \"My_Table\" (id integer NOT NULL)",
            "ALTER TABLE ONLY \"my_table\" ADD CONSTRAINT pk PRIMARY KEY (id)",
        ]);
        assert!(detect(&stmts, &PK_KIND).is_empty());
    }

    #[test]
    fn multi_op_alter_is_left_alone() {
        let stmts = parse_ok(&[
            "CREATE TABLE public.t (a integer, b integer)",
            "ALTER TABLE ONLY public.t \
                ADD CONSTRAINT pk PRIMARY KEY (a), \
                ADD CONSTRAINT u UNIQUE (b)",
        ]);
        assert!(detect(&stmts, &PK_KIND).is_empty());
    }

    #[test]
    fn exact_schema_match_preferred_over_wildcard() {
        // Qualified ALTER must correlate with qualified CREATE — not a
        // wildcard hit on the unqualified one that appeared first.
        let stmts = parse_ok(&[
            "CREATE TABLE t (id integer)",
            "CREATE TABLE public.t (id integer)",
            "ALTER TABLE ONLY public.t ADD CONSTRAINT pk PRIMARY KEY (id)",
        ]);
        let idioms = detect(&stmts, &PK_KIND);
        assert_eq!(idioms.len(), 1);
        assert_eq!(idioms[0].create_table_index, 1);
        assert_eq!(idioms[0].table, (Some("public".into()), "t".into()));
    }

    /// PK INCLUDE round-trips. Pinned because dropping INCLUDE silently
    /// changes the declared schema (covering columns lost) yet produces
    /// still-valid SQL the cluster would accept.
    #[test]
    fn pk_include_columns_round_trip() {
        let stmts = parse_ok(&[
            "CREATE TABLE t (id integer NOT NULL, payload text)",
            "ALTER TABLE ONLY t ADD CONSTRAINT t_pk PRIMARY KEY (id) INCLUDE (payload)",
        ]);
        let idioms = detect(&stmts, &PK_KIND);
        assert_eq!(idioms.len(), 1);
        let include: Vec<String> = idioms[0]
            .constraint
            .include
            .iter()
            .map(|i| i.value.clone())
            .collect();
        assert_eq!(include, vec!["payload".to_string()]);
    }

    /// UNIQUE INCLUDE round-trips. Cheap insurance against the upstream
    /// `PrimaryKeyConstraint` and `UniqueConstraint` AST types diverging
    /// — they share an `include` field today but separate test pins it.
    #[test]
    fn unique_include_columns_round_trip() {
        let stmts = parse_ok(&[
            "CREATE TABLE t (id integer, email text NOT NULL, payload text)",
            "ALTER TABLE ONLY t ADD CONSTRAINT t_uk UNIQUE (email) INCLUDE (payload)",
        ]);
        let idioms = detect(&stmts, &UNIQUE_KIND);
        assert_eq!(idioms.len(), 1);
        let include: Vec<String> = idioms[0]
            .constraint
            .include
            .iter()
            .map(|i| i.value.clone())
            .collect();
        assert_eq!(include, vec!["payload".to_string()]);
    }
}
