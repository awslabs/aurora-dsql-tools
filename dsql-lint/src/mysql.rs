//! MySQL → DSQL DDL translation.
//!
//! `fix_sql_mysql` parses MySQL-dialect DDL (mysqldump `CREATE TABLE` output)
//! with sqlparser's `MySqlDialect`, normalizes the MySQL-specific AST into
//! Postgres-shaped AST, re-emits Postgres SQL, then delegates to the existing
//! [`crate::fix_sql`] as the shared final DSQL-compatibility gate. The
//! Postgres pipeline is untouched: MySQL knowledge lives entirely in the
//! normalize pass here.

use sqlparser::ast::{
    CreateTableOptions, Expr, Ident, IndexColumn, KeyOrIndexDisplay, ObjectName, ObjectNamePart,
    Statement, TableConstraint,
};
use sqlparser::dialect::MySqlDialect;
use sqlparser::parser::Parser;

use crate::lint::{FixOutput, fix_sql};

/// Translate MySQL-dialect DDL to DSQL-compatible SQL.
///
/// Parses with `MySqlDialect`, normalizes MySQL-isms in the AST to
/// Postgres-shaped nodes, re-emits Postgres SQL, then runs the existing
/// [`fix_sql`] as the shared DSQL gate. Mirrors [`fix_sql`]'s signature so
/// callers (the loader's migrate path) pick it by source dialect with no
/// other change.
///
/// If the MySQL parse fails, the input is forwarded to [`fix_sql`] unchanged,
/// so the caller still gets a `ParseError` diagnostic from the Postgres path
/// rather than a silent empty result.
pub fn fix_sql_mysql(sql: &str) -> FixOutput {
    let parsed = match Parser::parse_sql(&MySqlDialect {}, sql) {
        Ok(stmts) => stmts,
        Err(_) => return fix_sql(sql),
    };

    let normalized: Vec<String> = parsed
        .into_iter()
        .map(|mut stmt| {
            normalize_statement(&mut stmt);
            format!("{stmt}")
        })
        .collect();

    fix_sql(&join_statements(&normalized))
}

/// Join normalized statements into a single SQL string for `fix_sql`.
fn join_statements(stmts: &[String]) -> String {
    let mut out = stmts.join(";\n");
    if !out.is_empty() {
        out.push(';');
    }
    out
}

/// Rewrite one MySQL-dialect statement into Postgres-shaped AST in place.
fn normalize_statement(stmt: &mut Statement) {
    if let Statement::CreateTable(ct) = stmt {
        unquote_object_name(&mut ct.name);
        for col in &mut ct.columns {
            unquote_ident(&mut col.name);
        }
        for constraint in &mut ct.constraints {
            unquote_constraint(constraint);
        }
        // ENGINE=, DEFAULT CHARSET=, COLLATE=, ROW_FORMAT, table COMMENT, etc.
        // have no DSQL meaning — drop them wholesale.
        ct.table_options = CreateTableOptions::None;
    }
}

/// Strip backtick quoting from every identifier a table constraint carries:
/// its optional constraint/index name and its column list. Backticks in
/// constraints are the first thing the Postgres `fix_sql` parse rejects.
fn unquote_constraint(constraint: &mut TableConstraint) {
    // MySQL writes a table-level unique as `UNIQUE KEY <index_name> (cols)`,
    // which Postgres rejects. Drop the `KEY` display word and promote the
    // index name to the constraint name → `CONSTRAINT <name> UNIQUE (cols)`.
    if let TableConstraint::Unique(c) = constraint {
        c.index_type_display = KeyOrIndexDisplay::None;
        if c.name.is_none() {
            c.name = c.index_name.take();
        } else {
            c.index_name = None;
        }
    }

    let (name, index_name, columns): (
        &mut Option<Ident>,
        Option<&mut Option<Ident>>,
        &mut [IndexColumn],
    ) = match constraint {
        TableConstraint::PrimaryKey(c) => (&mut c.name, Some(&mut c.index_name), &mut c.columns),
        TableConstraint::Unique(c) => (&mut c.name, Some(&mut c.index_name), &mut c.columns),
        TableConstraint::Index(c) => (&mut c.name, None, &mut c.columns),
        TableConstraint::ForeignKey(c) => {
            unquote_opt_ident(&mut c.name);
            unquote_object_name(&mut c.foreign_table);
            for col in &mut c.columns {
                unquote_ident(col);
            }
            for col in &mut c.referred_columns {
                unquote_ident(col);
            }
            return;
        }
        TableConstraint::Check(c) => {
            unquote_opt_ident(&mut c.name);
            return;
        }
        // Remaining variants (FulltextOrSpatial, *UsingIndex) carry idents too,
        // but mysqldump's default CREATE TABLE does not emit them. Leave them
        // for fix_sql to reject explicitly rather than silently half-handle.
        _ => return,
    };
    unquote_opt_ident(name);
    if let Some(index_name) = index_name {
        unquote_opt_ident(index_name);
    }
    for col in columns {
        unquote_index_column(col);
    }
}

/// Strip backticks from the identifiers inside one indexed-column expression.
fn unquote_index_column(col: &mut IndexColumn) {
    if let Expr::Identifier(ident) = &mut col.column.expr {
        unquote_ident(ident);
    }
}

fn unquote_opt_ident(ident: &mut Option<Ident>) {
    if let Some(ident) = ident {
        unquote_ident(ident);
    }
}

/// Strip the backtick quote style from every part of an object name so Display
/// emits a bare (or Postgres-double-quoted, if re-added later) identifier
/// instead of a MySQL backtick identifier.
fn unquote_object_name(name: &mut ObjectName) {
    for part in &mut name.0 {
        if let ObjectNamePart::Identifier(ident) = part {
            unquote_ident(ident);
        }
    }
}

/// Drop a backtick quote style from one identifier. Postgres folds unquoted
/// identifiers to lower case; mysqldump already emits the canonical case, so
/// leaving it unquoted matches the source table/column names in practice.
fn unquote_ident(ident: &mut Ident) {
    if ident.quote_style == Some('`') {
        ident.quote_style = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// No backtick may survive anywhere in the output — not in the table name,
    /// columns, or constraint column lists — or the Postgres `fix_sql` parse
    /// fails.
    fn assert_no_backticks_and_no_parse_error(out: &FixOutput) {
        assert!(!out.sql.contains('`'), "backticks survived:\n{}", out.sql);
        assert!(
            !out.diagnostics
                .iter()
                .any(|d| matches!(d.rule, crate::LintRule::ParseError)),
            "Postgres parse failed on translated output:\n{}\ndiagnostics: {:?}",
            out.sql,
            out.diagnostics
        );
    }

    #[test]
    fn strips_backticks_and_engine_into_clean_postgres() {
        let sql = "CREATE TABLE `users` (`id` int NOT NULL, `name` varchar(255)) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;";
        let out = fix_sql_mysql(sql);
        assert_no_backticks_and_no_parse_error(&out);
        assert!(
            !out.sql.to_uppercase().contains("ENGINE"),
            "ENGINE= must be stripped, got:\n{}",
            out.sql
        );
        assert!(
            out.sql.to_uppercase().contains("USERS"),
            "should still be a CREATE TABLE for users, got:\n{}",
            out.sql
        );
    }

    /// Backticks inside inline constraint column lists (PRIMARY KEY, KEY) must
    /// also be stripped — this was the first ParseError observed end-to-end.
    #[test]
    fn strips_backticks_inside_constraints() {
        let sql = "CREATE TABLE `t` (`id` int NOT NULL, `name` varchar(50), \
                   PRIMARY KEY (`id`), UNIQUE KEY `uk` (`name`)) ENGINE=InnoDB;";
        let out = fix_sql_mysql(sql);
        assert_no_backticks_and_no_parse_error(&out);
        assert!(
            out.sql.to_uppercase().contains("PRIMARY KEY"),
            "PRIMARY KEY must survive, got:\n{}",
            out.sql
        );
    }
}
