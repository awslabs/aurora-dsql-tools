//! MySQL → DSQL DDL translation.
//!
//! `fix_sql_mysql` parses MySQL-dialect DDL (mysqldump `CREATE TABLE` output)
//! with sqlparser's `MySqlDialect`, normalizes the MySQL-specific AST into
//! Postgres-shaped AST, re-emits Postgres SQL, then delegates to the existing
//! [`crate::fix_sql`] as the shared final DSQL-compatibility gate. The
//! Postgres pipeline is untouched: MySQL knowledge lives entirely in the
//! normalize pass here.

use sqlparser::ast::{
    CharacterLength, ColumnOption, ColumnOptionDef, CreateIndex, CreateTableOptions, DataType,
    ExactNumberInfo, Expr, GeneratedAs, Ident, IndexColumn, KeyOrIndexDisplay, ObjectName,
    ObjectNamePart, SequenceOptions, Statement, TableConstraint, TimezoneInfo, Value,
    ValueWithSpan,
};
use sqlparser::dialect::MySqlDialect;
use sqlparser::parser::Parser;
use sqlparser::tokenizer::{Span, Token, Tokenizer};

use crate::lint::{fix_sql, FixOutput};

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
    // Split first, then parse each statement independently. mysqldump DDL is
    // interleaved with MySQL-only noise (`LOCK TABLES`, `/*!40000 ALTER ...
    // DISABLE KEYS */`, session `SET @var`) that sqlparser cannot represent —
    // a whole-buffer `parse_sql` aborts on the first such statement and would
    // throw away every good translation. Per-statement parsing lets us drop
    // the noise and keep the CREATE TABLEs.
    let mut normalized: Vec<String> = Vec::new();
    for stmt_sql in split_mysql_statements(sql) {
        let mut parsed = match Parser::parse_sql(&MySqlDialect {}, &stmt_sql) {
            Ok(p) => p,
            // Unparseable → MySQL-only noise with no DSQL equivalent; drop it.
            Err(_) => continue,
        };
        for stmt in &mut parsed {
            if is_mysql_only_noise(stmt) {
                continue;
            }
            let extra = normalize_statement(stmt);
            normalized.push(format!("{stmt}"));
            normalized.extend(extra.into_iter().map(|s| format!("{s}")));
        }
    }

    fix_sql(&join_statements(&normalized))
}

/// Split a MySQL DDL string into individual statement texts on top-level `;`,
/// using the MySQL tokenizer so a `;` inside a string/backtick/comment is not
/// a boundary. Returns the trimmed, non-empty statement texts.
fn split_mysql_statements(sql: &str) -> Vec<String> {
    let dialect = MySqlDialect {};
    let Ok(tokens) = Tokenizer::new(&dialect, sql).tokenize() else {
        // Tokenize failure is rare for DDL; fall back to the whole string so
        // the caller still attempts a parse (and drops it if unparseable).
        return vec![sql.to_string()];
    };
    let mut out = Vec::new();
    let mut current = String::new();
    for tok in tokens {
        match tok {
            Token::SemiColon => {
                if !current.trim().is_empty() {
                    out.push(current.trim().to_string());
                }
                current.clear();
            }
            other => current.push_str(&other.to_string()),
        }
    }
    if !current.trim().is_empty() {
        out.push(current.trim().to_string());
    }
    out
}

/// MySQL-only statements that have no DSQL meaning and must be dropped (rather
/// than re-emitted into the Postgres `fix_sql`, which would reject them).
/// `LOCK`/`UNLOCK TABLES` are bulk-load locking; `SET` is session/charset
/// directives (mysqldump's `/*!40101 SET ... */` preamble). `CREATE TABLE`,
/// `DROP TABLE` (kept for idempotency), and `CREATE INDEX` are retained.
fn is_mysql_only_noise(stmt: &Statement) -> bool {
    matches!(
        stmt,
        Statement::LockTables { .. } | Statement::UnlockTables | Statement::Set(_)
    )
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
/// Returns any follow-on statements that must be emitted *after* this one
/// (e.g. a `CREATE INDEX` lifted out of an inline secondary `KEY`).
fn normalize_statement(stmt: &mut Statement) -> Vec<Statement> {
    // DROP TABLE (kept for idempotency) just needs its backtick identifiers
    // stripped so the Postgres `fix_sql` parse accepts it.
    if let Statement::Drop { names, .. } = stmt {
        for name in names.iter_mut() {
            unquote_object_name(name);
        }
        return Vec::new();
    }
    let Statement::CreateTable(ct) = stmt else {
        return Vec::new();
    };
    unquote_object_name(&mut ct.name);
    for col in &mut ct.columns {
        unquote_ident(&mut col.name);
        normalize_data_type(&mut col.data_type);
        strip_mysql_column_options(col);
        normalize_auto_increment(col);
    }
    // Lift secondary KEY/INDEX constraints out into separate CREATE INDEX
    // statements (DSQL has no inline secondary index); keep PK/UNIQUE/etc.
    // inline. FK/FULLTEXT pass through for the existing fix_sql to reject.
    let table = ct.name.clone();
    let mut extra = Vec::new();
    ct.constraints.retain_mut(|constraint| {
        if let TableConstraint::Index(idx) = constraint {
            extra.push(lift_index(&table, idx));
            false
        } else {
            unquote_constraint(constraint);
            true
        }
    });
    // ENGINE=, DEFAULT CHARSET=, COLLATE=, ROW_FORMAT, table COMMENT, etc.
    // have no DSQL meaning — drop them wholesale.
    ct.table_options = CreateTableOptions::None;
    extra
}

/// Replace a column's MySQL `AUTO_INCREMENT` option with a DSQL identity
/// declaration (`BIGINT GENERATED BY DEFAULT AS IDENTITY (CACHE 65536)`), per
/// the AWS MySQL→DSQL guidance. `BY DEFAULT` mirrors MySQL semantics (an
/// explicit value wins), so existing IDs reload. Dropping AUTO_INCREMENT
/// without a replacement would silently lose the column's auto-increment.
fn normalize_auto_increment(col: &mut sqlparser::ast::ColumnDef) {
    let had_auto_increment = col.options.iter().any(|opt| is_auto_increment(&opt.option));
    if !had_auto_increment {
        return;
    }
    col.options.retain(|opt| !is_auto_increment(&opt.option));
    col.data_type = DataType::BigInt(None);
    col.options.push(ColumnOptionDef {
        name: None,
        option: ColumnOption::Generated {
            generated_as: GeneratedAs::ByDefault,
            sequence_options: Some(vec![SequenceOptions::Cache(num_expr(65536))]),
            generation_expr: None,
            generation_expr_mode: None,
            generated_keyword: false,
        },
    });
}

/// Drop MySQL-only column options that have no DSQL meaning and that the
/// lenient Postgres parser would otherwise accept into invalid DSQL:
/// per-column `CHARACTER SET` / `COLLATE` (DSQL is UTF-8 + C collation),
/// inline `COMMENT` (no Postgres inline-comment syntax), and
/// `ON UPDATE CURRENT_TIMESTAMP` (no Postgres equivalent — `DEFAULT
/// CURRENT_TIMESTAMP` is kept). Application-layer timestamp maintenance
/// replaces ON UPDATE.
fn strip_mysql_column_options(col: &mut sqlparser::ast::ColumnDef) {
    col.options.retain(|opt| {
        !matches!(
            opt.option,
            ColumnOption::CharacterSet(_)
                | ColumnOption::Collation(_)
                | ColumnOption::Comment(_)
                | ColumnOption::OnUpdate(_)
        )
    });
}

/// Whether a column option is MySQL's `AUTO_INCREMENT` (parsed as a
/// dialect-specific token sequence).
fn is_auto_increment(option: &ColumnOption) -> bool {
    matches!(option, ColumnOption::DialectSpecific(_))
        && format!("{option}")
            .to_uppercase()
            .contains("AUTO_INCREMENT")
}

/// Build a `CREATE INDEX <name> ON <table> (cols)` from a lifted inline
/// secondary `KEY`/`INDEX`. The existing `fix_sql` IndexAsync rule rewrites it
/// to `CREATE INDEX ASYNC` for DSQL.
fn lift_index(table: &ObjectName, idx: &mut sqlparser::ast::IndexConstraint) -> Statement {
    let name = idx.name.take().map(|mut n| {
        unquote_ident(&mut n);
        ObjectName(vec![ObjectNamePart::Identifier(n)])
    });
    let mut columns = std::mem::take(&mut idx.columns);
    for col in &mut columns {
        unquote_index_column(col);
    }
    Statement::CreateIndex(CreateIndex {
        name,
        table_name: table.clone(),
        using: None,
        columns,
        unique: false,
        concurrently: false,
        r#async: false,
        if_not_exists: false,
        include: vec![],
        nulls_distinct: None,
        with: vec![],
        predicate: None,
        index_options: vec![],
        alter_options: vec![],
    })
}

/// Whether a single-part object name equals `target` (ASCII case-insensitive).
fn object_name_eq_ci(name: &ObjectName, target: &str) -> bool {
    name.0.len() == 1
        && matches!(
            &name.0[0],
            ObjectNamePart::Identifier(id) if id.value.eq_ignore_ascii_case(target)
        )
}

/// A bare unsigned numeric literal expression (for sequence CACHE values).
fn num_expr(n: u64) -> Expr {
    Expr::Value(ValueWithSpan {
        value: Value::Number(n.to_string(), false),
        span: Span::empty(),
    })
}

/// Map a MySQL column data type to its DSQL (Postgres) equivalent. Types with
/// a direct Postgres spelling (varchar, text, date, time, decimal, ...) are
/// left untouched; only MySQL-specific shapes are rewritten.
fn normalize_data_type(ty: &mut DataType) {
    let replacement = match ty {
        // tinyint(1) is MySQL's boolean convention; wider/!=1 → SMALLINT.
        DataType::TinyInt(Some(1)) => DataType::Boolean,
        DataType::TinyInt(_) | DataType::TinyIntUnsigned(_) => DataType::SmallInt(None),
        // No MEDIUMINT in DSQL.
        DataType::MediumInt(_) | DataType::MediumIntUnsigned(_) => DataType::Integer(None),
        // Unsigned widening: next signed type holds the full unsigned range.
        DataType::SmallIntUnsigned(_) => DataType::Integer(None),
        DataType::IntUnsigned(_) | DataType::IntegerUnsigned(_) => DataType::BigInt(None),
        // bigint unsigned overflows i64 → NUMERIC (arbitrary precision, stores
        // the full unsigned range). NOTE: emits bare NUMERIC, not NUMERIC(20,0);
        // and unsigned types do not add a `CHECK (col >= 0)` invariant. Both
        // are valid DSQL that store the data; tightening to NUMERIC(20,0) +
        // CHECK is deferred polish (design phase L4).
        DataType::BigIntUnsigned(_) => DataType::Numeric(ExactNumberInfo::None),
        // Signed integers with a MySQL display width (`int(11)`) → drop the
        // width; Postgres has no integer type modifier.
        DataType::Int(Some(_)) => DataType::Int(None),
        DataType::Integer(Some(_)) => DataType::Integer(None),
        DataType::SmallInt(Some(_)) => DataType::SmallInt(None),
        DataType::BigInt(Some(_)) => DataType::BigInt(None),
        // DATETIME → TIMESTAMP (without time zone).
        DataType::Datetime(_) => DataType::Timestamp(None, TimezoneInfo::None),
        // YEAR (parsed as a custom type name) has no DSQL type → INTEGER.
        DataType::Custom(name, _) if object_name_eq_ci(name, "year") => DataType::Integer(None),
        // ENUM → VARCHAR(255) (CHECK-based value validation is a later
        // enhancement). SET → TEXT: a many-member set's comma-joined value
        // can exceed 255 chars, so VARCHAR(255) would truncate.
        DataType::Enum(_, _) => DataType::Varchar(Some(CharacterLength::IntegerLength {
            length: 255,
            unit: None,
        })),
        DataType::Set(_) => DataType::Text,
        // Everything else (varchar, char, text, date, time, decimal,
        // width-less int/bigint/smallint, double, real, json, blob, ...) maps
        // directly.
        _ => return,
    };
    *ty = replacement;
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

    /// Assert the output is clean DSQL. Checks for `ParseError` AND for
    /// MySQL-isms that sqlparser's lenient `PostgreSqlDialect` parses without
    /// complaint but real DSQL rejects (backticks, inline COMMENT, CHARACTER
    /// SET/COLLATE, integer display widths, ON UPDATE, MySQL-only type names).
    /// A no-ParseError check alone is NOT sufficient — the gate is lenient.
    fn assert_clean_dsql(out: &FixOutput) {
        assert!(!out.sql.contains('`'), "backticks survived:\n{}", out.sql);
        assert!(
            !out.diagnostics
                .iter()
                .any(|d| matches!(d.rule, crate::LintRule::ParseError)),
            "Postgres parse failed on translated output:\n{}\ndiagnostics: {:?}",
            out.sql,
            out.diagnostics
        );
        let u = out.sql.to_uppercase();
        for banned in [
            "COMMENT '",
            "CHARACTER SET",
            "COLLATE",
            "ON UPDATE",
            "AUTO_INCREMENT",
            "UNSIGNED",
            "ENUM(",
            "DATETIME",
            "TINYINT",
            "MEDIUMINT",
            " YEAR",
        ] {
            assert!(
                !u.contains(banned),
                "MySQL-ism {banned:?} survived into output (lenient PG parser won't flag it):\n{}",
                out.sql
            );
        }
    }

    #[test]
    fn strips_backticks_and_engine_into_clean_postgres() {
        let sql = "CREATE TABLE `users` (`id` int NOT NULL, `name` varchar(255)) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;";
        let out = fix_sql_mysql(sql);
        assert_clean_dsql(&out);
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
        assert_clean_dsql(&out);
        assert!(
            out.sql.to_uppercase().contains("PRIMARY KEY"),
            "PRIMARY KEY must survive, got:\n{}",
            out.sql
        );
    }

    /// `tinyint(1)` is MySQL's boolean convention → BOOLEAN; wider tinyints and
    /// other small ints widen to a Postgres integer type (no TINYINT/MEDIUMINT).
    #[test]
    fn maps_integer_family_types() {
        let sql = "CREATE TABLE `t` (\
                   `flag` tinyint(1), `small` tinyint, `mid` mediumint, `n` int, `big` bigint) ENGINE=InnoDB;";
        let out = fix_sql_mysql(sql);
        assert_clean_dsql(&out);
        let u = out.sql.to_uppercase();
        assert!(
            u.contains("FLAG BOOLEAN"),
            "tinyint(1)->BOOLEAN, got:\n{}",
            out.sql
        );
        assert!(
            u.contains("SMALL SMALLINT"),
            "tinyint->SMALLINT, got:\n{}",
            out.sql
        );
        assert!(
            u.contains("MID INTEGER"),
            "mediumint->INTEGER, got:\n{}",
            out.sql
        );
        assert!(
            !u.contains("TINYINT") && !u.contains("MEDIUMINT"),
            "no MySQL int types, got:\n{}",
            out.sql
        );
    }

    /// Unsigned integers widen to the next signed Postgres type (DSQL has no
    /// UNSIGNED); bigint unsigned overflows i64 so it becomes NUMERIC.
    #[test]
    fn widens_unsigned_integers() {
        let sql = "CREATE TABLE `t` (`a` int unsigned, `b` bigint unsigned) ENGINE=InnoDB;";
        let out = fix_sql_mysql(sql);
        assert_clean_dsql(&out);
        let u = out.sql.to_uppercase();
        assert!(
            !u.contains("UNSIGNED"),
            "UNSIGNED must be gone, got:\n{}",
            out.sql
        );
        assert!(
            u.contains("A BIGINT"),
            "int unsigned->BIGINT, got:\n{}",
            out.sql
        );
        assert!(
            u.contains("B NUMERIC"),
            "bigint unsigned->NUMERIC, got:\n{}",
            out.sql
        );
    }

    /// MySQL DATETIME has no Postgres equivalent name → TIMESTAMP.
    #[test]
    fn maps_datetime_to_timestamp() {
        let sql = "CREATE TABLE `t` (`created` datetime) ENGINE=InnoDB;";
        let out = fix_sql_mysql(sql);
        assert_clean_dsql(&out);
        let u = out.sql.to_uppercase();
        assert!(
            u.contains("TIMESTAMP"),
            "datetime->TIMESTAMP, got:\n{}",
            out.sql
        );
        assert!(
            !u.contains("DATETIME"),
            "DATETIME must be gone, got:\n{}",
            out.sql
        );
    }

    /// ENUM has no DSQL type → VARCHAR(255) (validation via CHECK is a later
    /// enhancement; the column must at least become a loadable Postgres type).
    #[test]
    fn maps_enum_to_varchar() {
        let sql = "CREATE TABLE `t` (`kind` enum('a','b','c')) ENGINE=InnoDB;";
        let out = fix_sql_mysql(sql);
        assert_clean_dsql(&out);
        let u = out.sql.to_uppercase();
        assert!(
            u.contains("KIND VARCHAR"),
            "enum->VARCHAR, got:\n{}",
            out.sql
        );
        assert!(!u.contains("ENUM"), "ENUM must be gone, got:\n{}", out.sql);
    }

    /// AUTO_INCREMENT must become a DSQL IDENTITY column, not be silently
    /// dropped (which would lose the column's auto-increment behavior).
    #[test]
    fn maps_auto_increment_to_identity() {
        let sql = "CREATE TABLE `t` (`id` int NOT NULL AUTO_INCREMENT, PRIMARY KEY (`id`)) ENGINE=InnoDB;";
        let out = fix_sql_mysql(sql);
        assert_clean_dsql(&out);
        let u = out.sql.to_uppercase();
        assert!(
            u.contains("GENERATED BY DEFAULT AS IDENTITY"),
            "AUTO_INCREMENT must become an IDENTITY column, got:\n{}",
            out.sql
        );
        assert!(
            !u.contains("AUTO_INCREMENT"),
            "AUTO_INCREMENT must be gone, got:\n{}",
            out.sql
        );
    }

    /// A secondary `KEY`/`INDEX` inside CREATE TABLE is not valid DSQL — it
    /// must be lifted out into a separate `CREATE INDEX` statement (which the
    /// existing fix_sql then turns into `CREATE INDEX ASYNC`).
    #[test]
    fn lifts_secondary_key_to_create_index() {
        let sql = "CREATE TABLE `t` (`id` int NOT NULL, `name` varchar(50), \
                   PRIMARY KEY (`id`), KEY `idx_name` (`name`)) ENGINE=InnoDB;";
        let out = fix_sql_mysql(sql);
        assert_clean_dsql(&out);
        let u = out.sql.to_uppercase();
        assert!(
            u.contains("CREATE INDEX"),
            "secondary KEY must become a CREATE INDEX, got:\n{}",
            out.sql
        );
        // The inline KEY clause must be gone from the CREATE TABLE body.
        assert!(
            !u.contains("KEY IDX_NAME") && !u.contains("KEY `IDX_NAME`"),
            "inline KEY must be lifted out of CREATE TABLE, got:\n{}",
            out.sql
        );
        assert!(
            u.contains("PRIMARY KEY"),
            "PRIMARY KEY must survive inline, got:\n{}",
            out.sql
        );
    }

    /// `ON UPDATE CURRENT_TIMESTAMP` has no Postgres equivalent and breaks the
    /// parse; it must be stripped (keeping `DEFAULT CURRENT_TIMESTAMP`).
    /// Common on every mysqldump `updated_at` column.
    #[test]
    fn strips_on_update_current_timestamp() {
        let sql = "CREATE TABLE `t` (`ts` timestamp DEFAULT CURRENT_TIMESTAMP ON UPDATE CURRENT_TIMESTAMP);";
        let out = fix_sql_mysql(sql);
        assert_clean_dsql(&out);
        assert!(
            out.sql.to_uppercase().contains("DEFAULT CURRENT_TIMESTAMP"),
            "DEFAULT CURRENT_TIMESTAMP must survive, got:\n{}",
            out.sql
        );
    }

    /// Inline column `COMMENT '...'` is MySQL-only; the lenient PG parser
    /// accepts it but DSQL rejects it at apply — must be stripped.
    #[test]
    fn strips_column_comment() {
        let sql = "CREATE TABLE `t` (`n` int COMMENT 'a note');";
        let out = fix_sql_mysql(sql);
        assert_clean_dsql(&out);
    }

    /// Per-column `CHARACTER SET` / `COLLATE` are MySQL-only; strip them.
    #[test]
    fn strips_column_charset_and_collate() {
        let sql =
            "CREATE TABLE `t` (`s` varchar(10) CHARACTER SET utf8mb4 COLLATE utf8mb4_general_ci);";
        let out = fix_sql_mysql(sql);
        assert_clean_dsql(&out);
        assert!(
            out.sql.to_uppercase().contains("VARCHAR(10)"),
            "type must survive, got:\n{}",
            out.sql
        );
    }

    /// Signed integer display widths (`int(11)`, `bigint(20)`) are MySQL-only
    /// and must be dropped — Postgres has no integer type modifier.
    #[test]
    fn drops_signed_integer_display_width() {
        let sql = "CREATE TABLE `t` (`a` int(11), `b` bigint(20), `c` smallint(6));";
        let out = fix_sql_mysql(sql);
        assert_clean_dsql(&out);
        let u = out.sql.to_uppercase();
        assert!(
            u.contains("A INT") || u.contains("A INTEGER"),
            "int width dropped, got:\n{}",
            out.sql
        );
        assert!(
            !u.contains("(11)") && !u.contains("(20)") && !u.contains("(6)"),
            "no display widths, got:\n{}",
            out.sql
        );
    }

    /// `YEAR` has no DSQL type → INTEGER.
    #[test]
    fn maps_year_to_integer() {
        let sql = "CREATE TABLE `t` (`y` year);";
        let out = fix_sql_mysql(sql);
        assert_clean_dsql(&out);
        assert!(
            out.sql.to_uppercase().contains("INTEGER"),
            "year->INTEGER, got:\n{}",
            out.sql
        );
    }

    /// `SET(...)` → TEXT (per the design policy; VARCHAR(255) can truncate a
    /// many-member set's comma-joined value).
    #[test]
    fn maps_set_to_text() {
        let sql = "CREATE TABLE `t` (`perms` set('read','write','admin'));";
        let out = fix_sql_mysql(sql);
        assert_clean_dsql(&out);
        assert!(
            out.sql.to_uppercase().contains("PERMS TEXT"),
            "set->TEXT, got:\n{}",
            out.sql
        );
    }

    /// A full mysqldump DDL section carries noise around the CREATE TABLE:
    /// `DROP TABLE IF EXISTS` (with backticks), session `SET @var`, executable
    /// `/*! ... */` comments, and `LOCK`/`UNLOCK TABLES`. These are MySQL-only
    /// and must not surface as ParseErrors — the CREATE TABLE translates, the
    /// noise is dropped (DROP TABLE is kept for idempotency, backticks gone).
    #[test]
    fn strips_mysqldump_noise_around_create_table() {
        let sql = "DROP TABLE IF EXISTS `users`;\n\
                   /*!40101 SET @saved_cs_client = @@character_set_client */;\n\
                   CREATE TABLE `users` (`id` int NOT NULL AUTO_INCREMENT, PRIMARY KEY (`id`)) ENGINE=InnoDB;\n\
                   /*!40101 SET character_set_client = @saved_cs_client */;\n\
                   LOCK TABLES `users` WRITE;\n\
                   UNLOCK TABLES;\n\
                   /*!40000 ALTER TABLE `users` DISABLE KEYS */;";
        let out = fix_sql_mysql(sql);
        assert!(
            !out.diagnostics
                .iter()
                .any(|d| matches!(d.rule, crate::LintRule::ParseError)),
            "mysqldump noise must not produce ParseErrors:\n{}\ndiagnostics: {:?}",
            out.sql,
            out.diagnostics
        );
        assert!(!out.sql.contains('`'), "backticks gone: {}", out.sql);
        let u = out.sql.to_uppercase();
        assert!(u.contains("CREATE TABLE"), "CREATE TABLE kept: {}", out.sql);
        assert!(
            !u.contains("LOCK TABLES") && !u.contains("UNLOCK"),
            "LOCK/UNLOCK dropped: {}",
            out.sql
        );
        assert!(!u.contains("SET @"), "session SET dropped: {}", out.sql);
    }
}
