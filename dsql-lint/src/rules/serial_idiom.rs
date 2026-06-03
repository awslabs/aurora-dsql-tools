//! Detection (only) of the pg_dump "SERIAL idiom".
//!
//! A real PostgreSQL `pg_dump` expands a `SERIAL`/`bigserial` column into four
//! separate statements:
//!
//! ```sql
//! CREATE TABLE public.t (id integer NOT NULL, x text);
//! CREATE SEQUENCE public.t_id_seq START WITH 1 INCREMENT BY 1 NO MINVALUE NO MAXVALUE CACHE 1;
//! ALTER SEQUENCE public.t_id_seq OWNED BY public.t.id;
//! ALTER TABLE ONLY public.t ALTER COLUMN id SET DEFAULT nextval('public.t_id_seq'::regclass);
//! ```
//!
//! This module DETECTS that idiom and returns structured info about it. It does
//! NOT rewrite anything — a later pass will collapse the idiom into an inline
//! identity column.
//!
//! ## Index space
//! Detection operates only on the PARSEABLE statements. With sqlparser-dsql
//! 0.62.0, `ALTER SEQUENCE ... OWNED BY` does NOT parse, so callers pass only
//! the successfully-parsed statements. Every index in [`SerialIdiom`]
//! (`create_table_index`, `redundant_indices`) is an index into the `stmts`
//! slice handed to [`detect_serial_idioms`]. Matching the text-level removal of
//! the `OWNED BY` line is the next task's concern and is not handled here.

// Detection-only module: nothing in the crate calls these items yet. The
// collapse pass (next task) will wire `detect_serial_idioms` into `fix_sql`.
#![allow(dead_code)]

use sqlparser::ast::{
    AlterColumnOperation, AlterTableOperation, Expr, FunctionArg, FunctionArgExpr,
    FunctionArguments, ObjectName, Statement, Value, ValueWithSpan,
};

/// A normalized object reference: `(optional schema, name)` with quoting and
/// `::regclass` casts stripped, used to correlate the idiom's pieces.
type NameRef = (Option<String>, String);

/// A detected pg_dump SERIAL idiom: a CREATE TABLE column whose auto-increment
/// is wired up via a separate CREATE SEQUENCE + ALTER COLUMN SET DEFAULT nextval.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct SerialIdiom {
    /// Normalized (schema, table) of the CREATE TABLE.
    pub table: NameRef,
    /// Column name that should become an identity column.
    pub column: String,
    /// Normalized (schema, sequence_name).
    pub sequence: NameRef,
    /// Index (into the input statement slice) of the CREATE TABLE. The
    /// CREATE TABLE is rewritten (not removed), so it is tracked separately.
    pub create_table_index: usize,
    /// Indices (into the input statement slice) of the statements that make up
    /// the rest of the idiom and will later be removed: the CREATE SEQUENCE and
    /// the ALTER COLUMN SET DEFAULT.
    pub redundant_indices: Vec<usize>,
}

/// Normalize an `ObjectName` to `(Option<schema>, name)`, stripping quoting.
///
/// - 1 part  -> `(None, name)`
/// - 2 parts -> `(Some(schema), name)`
/// - 3+ parts -> the last two parts are treated as `(schema, name)` (handles
///   `db.schema.table` by keeping the trailing schema-qualified pair).
fn normalize_object_name(name: &ObjectName) -> Option<NameRef> {
    let idents: Vec<&str> = name
        .0
        .iter()
        .filter_map(|part| part.as_ident())
        .map(|ident| ident.value.as_str())
        .collect();
    match idents.as_slice() {
        [] => None,
        [n] => Some((None, (*n).to_string())),
        [.., schema, n] => Some((Some((*schema).to_string()), (*n).to_string())),
    }
}

/// Normalize a `schema.name` string (the unwrapped `nextval('...')` argument)
/// into `(Option<schema>, name)`.
fn normalize_dotted_str(s: &str) -> NameRef {
    match s.rsplit_once('.') {
        Some((schema, name)) => (Some(schema.to_string()), name.to_string()),
        None => (None, s.to_string()),
    }
}

/// Two normalized references match if their names are equal AND their schemas
/// agree where both are present. A missing schema on either side is treated as
/// a wildcard (pg_dump emits `public.t` in one place and `t` in another).
fn refs_match(a: &NameRef, b: &NameRef) -> bool {
    if a.1 != b.1 {
        return false;
    }
    match (&a.0, &b.0) {
        (Some(s1), Some(s2)) => s1 == s2,
        _ => true,
    }
}

/// Extract the referenced sequence name from a `nextval('seq'::regclass)` /
/// `nextval('seq')` expression. Returns the unwrapped, normalized sequence ref,
/// or `None` if the expression is not a recognizable `nextval(...)` call.
fn extract_nextval_sequence(expr: &Expr) -> Option<NameRef> {
    let Expr::Function(func) = expr else {
        return None;
    };
    // Function name must be `nextval` (case-insensitive), unqualified or
    // schema-qualified (e.g. `pg_catalog.nextval`).
    let fn_name = func.name.0.last().and_then(|p| p.as_ident())?;
    if !fn_name.value.eq_ignore_ascii_case("nextval") {
        return None;
    }

    let FunctionArguments::List(list) = &func.args else {
        return None;
    };
    let [FunctionArg::Unnamed(FunctionArgExpr::Expr(arg))] = list.args.as_slice() else {
        return None;
    };

    // The argument is the sequence name as a single-quoted string, possibly
    // wrapped in a `::regclass` cast.
    let str_expr = match arg {
        Expr::Cast { expr, .. } => expr.as_ref(),
        other => other,
    };
    if let Expr::Value(ValueWithSpan {
        value: Value::SingleQuotedString(s),
        ..
    }) = str_expr
    {
        Some(normalize_dotted_str(s))
    } else {
        None
    }
}

/// Detect all SERIAL idioms across a parsed statement list.
///
/// Reports an idiom only when ALL three linked pieces are present:
/// a `CREATE TABLE` for table T with column C, a
/// `CREATE SEQUENCE S`, and an
/// `ALTER TABLE T ALTER COLUMN C SET DEFAULT nextval('S'...)`. If any piece is
/// missing the candidate is dropped (it cannot be safely collapsed).
pub(crate) fn detect_serial_idioms(stmts: &[Statement]) -> Vec<SerialIdiom> {
    // CREATE TABLE: normalized (schema, table) -> (index, columns).
    let mut tables: Vec<(NameRef, usize, Vec<String>)> = Vec::new();
    // CREATE SEQUENCE: normalized (schema, name) -> index.
    let mut sequences: Vec<(NameRef, usize)> = Vec::new();
    // SET DEFAULT nextval: (table_ref, column, sequence_ref, index).
    let mut set_defaults: Vec<(NameRef, String, NameRef, usize)> = Vec::new();

    for (idx, stmt) in stmts.iter().enumerate() {
        match stmt {
            Statement::CreateTable(ct) => {
                if let Some(table_ref) = normalize_object_name(&ct.name) {
                    let cols = ct.columns.iter().map(|c| c.name.value.clone()).collect();
                    tables.push((table_ref, idx, cols));
                }
            }
            Statement::CreateSequence { name, .. } => {
                if let Some(seq_ref) = normalize_object_name(name) {
                    sequences.push((seq_ref, idx));
                }
            }
            Statement::AlterTable(alter) => {
                let Some(table_ref) = normalize_object_name(&alter.name) else {
                    continue;
                };
                for op in &alter.operations {
                    if let AlterTableOperation::AlterColumn {
                        column_name,
                        op: AlterColumnOperation::SetDefault { value },
                    } = op
                    {
                        if let Some(seq_ref) = extract_nextval_sequence(value) {
                            set_defaults.push((
                                table_ref.clone(),
                                column_name.value.clone(),
                                seq_ref,
                                idx,
                            ));
                        }
                    }
                }
            }
            _ => {}
        }
    }

    let mut idioms = Vec::new();
    for (table_ref, column, seq_ref, set_default_idx) in &set_defaults {
        // Full-signature gate: the CREATE TABLE (with the column), the matching
        // CREATE SEQUENCE, and this SET DEFAULT must all be present and linked.
        let Some((tbl_ref_norm, create_table_index, _)) = tables
            .iter()
            .find(|(t, _, cols)| refs_match(t, table_ref) && cols.iter().any(|c| c == column))
        else {
            continue;
        };
        let Some((seq_ref_norm, sequence_index)) =
            sequences.iter().find(|(s, _)| refs_match(s, seq_ref))
        else {
            continue;
        };

        idioms.push(SerialIdiom {
            table: tbl_ref_norm.clone(),
            column: column.clone(),
            sequence: seq_ref_norm.clone(),
            create_table_index: *create_table_index,
            redundant_indices: vec![*sequence_index, *set_default_idx],
        });
    }
    idioms
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlparser::dialect::PostgreSqlDialect;
    use sqlparser::parser::Parser;

    /// Parse each statement individually and collect the successfully-parsed
    /// ones, mirroring how the caller will skip the unparseable
    /// `ALTER SEQUENCE ... OWNED BY` line.
    fn parse_ok(stmts: &[&str]) -> Vec<Statement> {
        let dialect = PostgreSqlDialect {};
        stmts
            .iter()
            .filter_map(|s| Parser::parse_sql(&dialect, s).ok())
            .flatten()
            .collect()
    }

    #[test]
    fn happy_path_detects_single_idiom() {
        let stmts = parse_ok(&[
            "CREATE TABLE public.t (id integer NOT NULL, x text)",
            "CREATE SEQUENCE public.t_id_seq START WITH 1 INCREMENT BY 1 CACHE 1",
            "ALTER TABLE ONLY public.t ALTER COLUMN id SET DEFAULT nextval('public.t_id_seq'::regclass)",
        ]);
        let idioms = detect_serial_idioms(&stmts);
        assert_eq!(idioms.len(), 1, "expected exactly 1 idiom: {idioms:?}");
        let i = &idioms[0];
        assert_eq!(i.table, (Some("public".to_string()), "t".to_string()));
        assert_eq!(i.column, "id");
        assert_eq!(
            i.sequence,
            (Some("public".to_string()), "t_id_seq".to_string())
        );
        assert_eq!(i.create_table_index, 0);
        // CREATE SEQUENCE at index 1, SET DEFAULT at index 2.
        assert_eq!(i.redundant_indices, vec![1, 2]);
    }

    #[test]
    fn schema_qualified_vs_unqualified_correlates() {
        // CREATE TABLE unqualified, nextval schema-qualified, sequence qualified.
        let stmts = parse_ok(&[
            "CREATE TABLE t (id integer NOT NULL, x text)",
            "CREATE SEQUENCE public.t_id_seq CACHE 1",
            "ALTER TABLE ONLY public.t ALTER COLUMN id SET DEFAULT nextval('public.t_id_seq'::regclass)",
        ]);
        let idioms = detect_serial_idioms(&stmts);
        assert_eq!(idioms.len(), 1, "expected exactly 1 idiom: {idioms:?}");
        assert_eq!(idioms[0].column, "id");
        assert_eq!(idioms[0].create_table_index, 0);
        assert_eq!(idioms[0].redundant_indices, vec![1, 2]);
    }

    #[test]
    fn free_standing_sequence_no_idiom() {
        // CREATE SEQUENCE with no matching SET DEFAULT -> nothing to collapse.
        let stmts = parse_ok(&[
            "CREATE TABLE public.t (id integer NOT NULL, x text)",
            "CREATE SEQUENCE public.t_id_seq CACHE 1",
        ]);
        let idioms = detect_serial_idioms(&stmts);
        assert_eq!(idioms.len(), 0, "expected 0 idioms: {idioms:?}");
    }

    #[test]
    fn set_default_referencing_absent_sequence_no_idiom() {
        // SET DEFAULT nextval('other_seq') but no CREATE SEQUENCE other_seq.
        let stmts = parse_ok(&[
            "CREATE TABLE public.t (id integer NOT NULL, x text)",
            "ALTER TABLE ONLY public.t ALTER COLUMN id SET DEFAULT nextval('public.other_seq'::regclass)",
        ]);
        let idioms = detect_serial_idioms(&stmts);
        assert_eq!(idioms.len(), 0, "expected 0 idioms: {idioms:?}");
    }

    #[test]
    fn set_default_not_nextval_no_idiom() {
        // SET DEFAULT 0 is not a nextval idiom, even with a sequence present.
        let stmts = parse_ok(&[
            "CREATE TABLE public.t (id integer NOT NULL, x text)",
            "CREATE SEQUENCE public.t_id_seq CACHE 1",
            "ALTER TABLE ONLY public.t ALTER COLUMN id SET DEFAULT 0",
        ]);
        let idioms = detect_serial_idioms(&stmts);
        assert_eq!(idioms.len(), 0, "expected 0 idioms: {idioms:?}");
    }

    #[test]
    fn two_tables_each_serial_detects_two() {
        let stmts = parse_ok(&[
            "CREATE TABLE public.a (id integer NOT NULL)",
            "CREATE SEQUENCE public.a_id_seq CACHE 1",
            "ALTER TABLE ONLY public.a ALTER COLUMN id SET DEFAULT nextval('public.a_id_seq'::regclass)",
            "CREATE TABLE public.b (id integer NOT NULL)",
            "CREATE SEQUENCE public.b_id_seq CACHE 1",
            "ALTER TABLE ONLY public.b ALTER COLUMN id SET DEFAULT nextval('public.b_id_seq'::regclass)",
        ]);
        let mut idioms = detect_serial_idioms(&stmts);
        assert_eq!(idioms.len(), 2, "expected 2 idioms: {idioms:?}");
        // Order by create_table_index for stable assertions.
        idioms.sort_by_key(|i| i.create_table_index);

        assert_eq!(idioms[0].table.1, "a");
        assert_eq!(idioms[0].create_table_index, 0);
        assert_eq!(idioms[0].redundant_indices, vec![1, 2]);

        assert_eq!(idioms[1].table.1, "b");
        assert_eq!(idioms[1].create_table_index, 3);
        assert_eq!(idioms[1].redundant_indices, vec![4, 5]);
    }
}
