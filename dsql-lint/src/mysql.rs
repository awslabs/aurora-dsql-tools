//! MySQL → DSQL DDL translation.
//!
//! `fix_sql_mysql` parses MySQL-dialect DDL (mysqldump `CREATE TABLE` output)
//! with sqlparser's `MySqlDialect`, normalizes the MySQL-specific AST into
//! Postgres-shaped AST, re-emits Postgres SQL, then delegates to the existing
//! [`crate::fix_sql`] as the shared final DSQL-compatibility gate. The
//! Postgres pipeline is untouched: MySQL knowledge lives entirely in the
//! normalize pass here.

use core::ops::ControlFlow;

use sqlparser::ast::{
    visit_expressions_mut, CharacterLength, ColumnOption, CreateIndex, CreateTableOptions,
    DataType, ExactNumberInfo, Expr, FunctionArg, FunctionArgExpr, FunctionArguments, GeneratedAs,
    Ident, IndexColumn, KeyOrIndexDisplay, ObjectName, ObjectNamePart, SequenceOptions, Statement,
    TableConstraint, TimezoneInfo, Value, ValueWithSpan,
};
use sqlparser::dialect::MySqlDialect;
use sqlparser::keywords::Keyword;
use sqlparser::parser::Parser;
use sqlparser::tokenizer::{Span, Token, Tokenizer};

use crate::lint::{
    fix_statements, split_statements_dialect, Diagnostic, FixOutput, FixResult, LintRule,
};
use crate::rules::errors::identity_with_cache;

/// Translate MySQL-dialect DDL to DSQL-compatible SQL.
///
/// Mirrors [`fix_sql`]'s signature for dialect dispatch. The source is split
/// into statements by *slicing the original bytes* (via
/// [`split_statements_dialect`] with `MySqlDialect`), never by rebuilding from
/// tokens — rebuilding double-unescapes string literals and silently corrupts
/// data. Each statement is parsed independently: a parse failure is dropped as
/// MySQL-only noise *unless* it opens a `CREATE TABLE`, in which case its raw
/// text is forwarded so the gate reports a `ParseError` rather than letting the
/// table vanish. Surviving statements are normalized to Postgres-shaped AST and
/// handed to [`fix_statements`] as the final DSQL gate — with their *original
/// source line numbers*, so gate diagnostics need no fragile post-hoc remap.
pub fn fix_sql_mysql(sql: &str) -> FixOutput {
    // (source_line, translated_statement_text) pairs fed to the gate. Slicing
    // from source (not token rebuild) keeps string literals byte-exact.
    let mut normalized: Vec<(usize, String)> = Vec::new();
    let mut mysql_diags: Vec<Diagnostic> = Vec::new();

    let stmts = split_statements_dialect(sql, &MySqlDialect {})
        .unwrap_or_else(|_| vec![(1, sql.to_string())]);

    for (line, stmt_sql) in stmts {
        let mut parsed = match Parser::parse_sql(&MySqlDialect {}, &stmt_sql) {
            Ok(p) => p,
            // An unparseable CREATE TABLE is a table we can't translate, not
            // noise — forward it so the gate reports a ParseError instead of
            // silently dropping the table. Other parse failures (`/*! ... */`
            // directives, out-of-scope CREATE VIEW/TRIGGER) are genuine noise.
            Err(_) => {
                if opens_create_table(&stmt_sql) {
                    normalized.push((line, strip_trailing_semicolon(&stmt_sql)));
                }
                continue;
            }
        };
        for stmt in &mut parsed {
            if is_mysql_only_noise(stmt) {
                continue;
            }
            // A DML statement (INSERT/UPDATE/...) is data, not DDL. Its MySQL
            // string escaping (backslash escapes) has no faithful Postgres
            // re-emission here, so translating it risks silent corruption.
            // Drop it and say so — never silently, never mangled. The loader
            // strips data before calling us; a raw CLI dump gets a clear signal.
            if is_dml(stmt) {
                mysql_diags.push(dml_dropped_warning(line, stmt, &stmt_sql));
                continue;
            }
            let before = mysql_diags.len();
            let extra = normalize_statement(stmt, &mut mysql_diags);
            // Stamp this statement's source line and text onto the warnings it
            // produced, so diagnostics point at the original MySQL file and JSON
            // consumers get a non-empty statement preview.
            for diag in &mut mysql_diags[before..] {
                diag.line = line;
                diag.statement = stmt_sql.clone();
            }
            normalized.push((line, stmt.to_string()));
            // A lifted CREATE INDEX inherits its CREATE TABLE's source line.
            normalized.extend(extra.into_iter().map(|s| (line, s.to_string())));
        }
    }

    // Shared DSQL gate, entered per-statement with true source lines. The
    // MySQL-translation warnings are prepended — a lossy transform stays a
    // `FixedWithWarning` the caller must review.
    let mut out = fix_statements(normalized);
    mysql_diags.extend(out.diagnostics);
    // One coordinate system, one order: diagnostics sorted by source line so
    // JSON/text consumers see them top-to-bottom regardless of producer.
    mysql_diags.sort_by_key(|d| d.line);
    out.diagnostics = mysql_diags;
    out
}

/// Build a `FixedWithWarning` diagnostic for a lossy MySQL→DSQL transform.
/// The caller stamps the source line after normalize_statement returns.
fn warn(rule: LintRule, message: &str, detail: String) -> Diagnostic {
    Diagnostic {
        rule,
        line: 0,
        statement: String::new(),
        message: message.to_string(),
        suggestion: "Review the translated column and adjust downstream expectations.".to_string(),
        fix_result: FixResult::FixedWithWarning(detail),
    }
}

/// A DML statement (INSERT/UPDATE/DELETE) was dropped, not translated. Reported
/// as `Unfixable` because there is no output for it — the user must load data
/// through the loader's data path, not this DDL translator.
fn dml_dropped_warning(line: usize, stmt: &Statement, stmt_sql: &str) -> Diagnostic {
    let kind = match stmt {
        Statement::Insert(_) => "INSERT",
        Statement::Update { .. } => "UPDATE",
        Statement::Delete(_) => "DELETE",
        _ => "DML",
    };
    Diagnostic {
        rule: LintRule::MysqlDataStatementDropped,
        line,
        statement: stmt_sql.to_string(),
        message: format!(
            "{kind} statement dropped: fix_sql_mysql translates DDL only, not row data."
        ),
        suggestion: "Load table data through the loader's data path; MySQL string escaping is not \
                     faithfully translatable to DSQL here."
            .to_string(),
        fix_result: FixResult::Unfixable,
    }
}

fn is_dml(stmt: &Statement) -> bool {
    matches!(
        stmt,
        Statement::Insert(_) | Statement::Update { .. } | Statement::Delete(_)
    )
}

fn strip_trailing_semicolon(s: &str) -> String {
    s.trim().trim_end_matches(';').trim_end().to_string()
}

/// Whether a parse-failing statement opens a `CREATE TABLE` — distinguishes an
/// untranslatable table (forwarded so the gate reports a ParseError) from
/// droppable MySQL noise. Detection is on the *token stream*, not the raw text:
/// mysqldump prefixes every table with a `-- Table structure for table x`
/// comment, and a text `strip_prefix("CREATE")` would miss it and drop the
/// whole table silently. `TABLE` must be the first object keyword after
/// `CREATE` and its modifiers, so a `CREATE ... VIEW ... FROM my_table` is not
/// mistaken for a table.
fn opens_create_table(stmt_sql: &str) -> bool {
    let Ok(tokens) = Tokenizer::new(&MySqlDialect {}, stmt_sql).tokenize() else {
        return false;
    };
    let mut words = tokens.iter().filter_map(|t| match t {
        Token::Word(w) => Some(w),
        _ => None,
    });
    if !words
        .next()
        .is_some_and(|w| w.keyword == Keyword::CREATE)
    {
        return false;
    }
    for w in words {
        // Skip modifier keywords (TEMPORARY, OR REPLACE, IF NOT EXISTS, and
        // mysqldump's view-definer clauses) until the first object keyword.
        match w.keyword {
            Keyword::TEMPORARY
            | Keyword::OR
            | Keyword::REPLACE
            | Keyword::IF
            | Keyword::NOT
            | Keyword::EXISTS
            | Keyword::DEFINER
            | Keyword::SQL
            | Keyword::SECURITY
            | Keyword::ALGORITHM
            | Keyword::UNDEFINED
            | Keyword::MERGE
            | Keyword::INVOKER => continue,
            Keyword::TABLE => return true,
            _ => return false,
        }
    }
    false
}

/// MySQL-only statements to drop (LOCK/UNLOCK/SET/USE) — Postgres `fix_sql`
/// would reject them. CREATE TABLE, DROP TABLE, and CREATE INDEX are retained.
fn is_mysql_only_noise(stmt: &Statement) -> bool {
    matches!(
        stmt,
        Statement::LockTables { .. }
            | Statement::UnlockTables
            | Statement::Set(_)
            | Statement::Use(_)
    )
}

/// Rewrite one MySQL-dialect statement into Postgres-shaped AST in place.
/// Returns any follow-on statements that must be emitted *after* this one
/// (e.g. a `CREATE INDEX` lifted out of an inline secondary `KEY`).
fn normalize_statement(stmt: &mut Statement, diags: &mut Vec<Diagnostic>) -> Vec<Statement> {
    // DROP TABLE is kept for idempotency; strip its backtick identifiers.
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
    // mysqldump records the table's next AUTO_INCREMENT value as a table option
    // (`... AUTO_INCREMENT=1001`). Capture it before the options are dropped so
    // the identity column can be seeded with `START WITH N` — otherwise the
    // sequence restarts at 1 and new inserts collide with reloaded rows.
    let auto_increment_seed = auto_increment_seed(&ct.table_options);
    for col in &mut ct.columns {
        let col_name = col.name.value.clone();
        unquote_ident(&mut col.name);
        // An AUTO_INCREMENT column becomes BIGINT identity regardless of its
        // declared type, so skip normalize_data_type — otherwise a type like
        // `bigint unsigned` would emit a "widened to NUMERIC" warning that
        // contradicts the BIGINT the identity rewrite actually produces.
        // bit(N)'s declared width is erased by the BYTEA rewrite but still
        // needed to pad a recast DEFAULT literal; capture it first. Clamp to
        // MySQL's max bit width (64): a malformed dump with `bit(2^64-1)` would
        // otherwise overflow the byte-count multiply and try to allocate a
        // gigantic padding string.
        let bit_width = match col.data_type {
            DataType::Bit(w) => w.map(|w| w.min(64)),
            _ => None,
        };
        if col.options.iter().any(|opt| is_auto_increment(&opt.option)) {
            normalize_auto_increment(col, &col_name, auto_increment_seed, diags);
        } else {
            normalize_data_type(&mut col.data_type, &col_name, diags);
        }
        normalize_default(col, &col_name, bit_width, diags);
        strip_mysql_column_options(col, &col_name, diags);
        unquote_column_option_exprs(col);
    }
    // Lift secondary KEY/INDEX constraints out into separate CREATE INDEX
    // statements (DSQL has no inline secondary index); keep PK/UNIQUE/etc.
    // inline. FK/FULLTEXT pass through for the existing fix_sql to reject.
    let table = ct.name.clone();
    let mut extra = Vec::new();
    ct.constraints.retain_mut(|constraint| {
        if let TableConstraint::Index(idx) = constraint {
            extra.push(lift_index(&table, idx, diags));
            false
        } else {
            unquote_constraint(constraint, diags);
            true
        }
    });
    // ENGINE=, DEFAULT CHARSET=, COLLATE=, ROW_FORMAT, table COMMENT, etc.
    // have no DSQL meaning — drop them wholesale.
    ct.table_options = CreateTableOptions::None;
    extra
}

/// Replace `AUTO_INCREMENT` with `BIGINT GENERATED BY DEFAULT AS IDENTITY
/// (CACHE 65536 [START WITH <seed>])`, per the AWS MySQL→DSQL guidance.
/// `BY DEFAULT` mirrors MySQL semantics (an explicit value wins), so existing
/// IDs reload. When mysqldump recorded the table's next AUTO_INCREMENT value,
/// `seed` carries it and the identity is seeded so new inserts continue past
/// the reloaded rows instead of restarting at 1.
fn normalize_auto_increment(
    col: &mut sqlparser::ast::ColumnDef,
    col_name: &str,
    seed: Option<u64>,
    diags: &mut Vec<Diagnostic>,
) {
    // Drop AUTO_INCREMENT and any DEFAULT: a column cannot carry both a DEFAULT
    // and GENERATED AS IDENTITY (Postgres/DSQL reject the pairing, and the
    // lenient PG gate won't flag it). MySQL forbids the pairing too, so nothing
    // faithful is lost.
    col.options.retain(|opt| {
        !is_auto_increment(&opt.option) && !matches!(opt.option, ColumnOption::Default(_))
    });
    col.data_type = DataType::BigInt(None);

    // Base identity shape shared with the Postgres SERIAL-idiom collapse
    // (CACHE 65536); append `START WITH <seed>` when the dump gave one.
    let mut identity = identity_with_cache(GeneratedAs::ByDefault, 65536);
    if let (
        Some(seed),
        ColumnOption::Generated {
            sequence_options: Some(opts),
            ..
        },
    ) = (seed, &mut identity.option)
    {
        opts.push(SequenceOptions::StartWith(num_expr(seed), true));
    }

    // When seeded, this is a faithful translation of the counter; otherwise it
    // resets to 1 and the caller must reset it before new inserts. Either way
    // it's lossy enough to warn — the sequence state, cycle behavior, and
    // per-connection allocation differ from MySQL.
    let detail = match seed {
        Some(seed) => format!(
            "Column `{col_name}`: AUTO_INCREMENT became BIGINT GENERATED BY DEFAULT AS IDENTITY \
             (CACHE 65536 START WITH {seed}), seeded from the dump's AUTO_INCREMENT={seed}. Verify \
             the seed is past your largest existing id before relying on new inserts."
        ),
        None => format!(
            "Column `{col_name}`: AUTO_INCREMENT became BIGINT GENERATED BY DEFAULT AS IDENTITY \
             (CACHE 65536). The dump carried no AUTO_INCREMENT seed, so the sequence starts at 1 — \
             after loading existing rows, reset it (ALTER TABLE ... ALTER COLUMN ... RESTART WITH N) \
             or new inserts may collide with existing ids."
        ),
    };
    diags.push(warn(
        LintRule::MysqlAutoIncrementToIdentity,
        "AUTO_INCREMENT translated to a DSQL identity column.",
        detail,
    ));
    col.options.push(identity);
}

/// Extract the `AUTO_INCREMENT=<n>` table option's seed value, if present.
/// mysqldump emits it as a plain space-separated key/value option; a value that
/// isn't a non-negative integer is ignored (no seed).
fn auto_increment_seed(options: &CreateTableOptions) -> Option<u64> {
    let opts = match options {
        CreateTableOptions::Plain(o) | CreateTableOptions::With(o) => o,
        _ => return None,
    };
    opts.iter().find_map(|opt| match opt {
        sqlparser::ast::SqlOption::KeyValue {
            key,
            value:
                Expr::Value(ValueWithSpan {
                    value: Value::Number(n, _),
                    ..
                }),
        } if key.value.eq_ignore_ascii_case("AUTO_INCREMENT") => n.parse().ok(),
        _ => None,
    })
}

/// Drop MySQL-only column options the lenient Postgres parser would otherwise
/// accept into invalid DSQL: `CHARACTER SET` / `COLLATE` / inline `COMMENT`
/// (cosmetic — DSQL is UTF-8 + C collation, so silent) and `ON UPDATE
/// CURRENT_TIMESTAMP` (lossy — the column stops auto-updating, so it warns;
/// `DEFAULT CURRENT_TIMESTAMP` is kept).
fn strip_mysql_column_options(
    col: &mut sqlparser::ast::ColumnDef,
    col_name: &str,
    diags: &mut Vec<Diagnostic>,
) {
    if col
        .options
        .iter()
        .any(|opt| matches!(opt.option, ColumnOption::OnUpdate(_)))
    {
        diags.push(warn(
            LintRule::MysqlOnUpdateDropped,
            "ON UPDATE CURRENT_TIMESTAMP dropped (no DSQL equivalent).",
            format!(
                "Column `{col_name}`: ON UPDATE CURRENT_TIMESTAMP was removed (DEFAULT \
                 CURRENT_TIMESTAMP kept). The column no longer auto-updates on write — replicate \
                 that in application code."
            ),
        ));
    }
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

/// Recast a column's DEFAULT after the type rewrite. Postgres type-checks
/// DEFAULT against the column type at CREATE time (MySQL doesn't), so a
/// default valid for the MySQL type can fail the whole CREATE TABLE. For
/// rewritten BOOLEAN/BYTEA columns only defaults provably valid survive:
/// recast where possible, dropped + warned otherwise — a shape we don't
/// recognize (`DEFAULT -1`, `DEFAULT (0)`) is invalid DSQL that the lenient
/// PG gate cannot catch. Date/time columns drop MySQL zero-date sentinels.
fn normalize_default(
    col: &mut sqlparser::ast::ColumnDef,
    col_name: &str,
    bit_width: Option<u64>,
    diags: &mut Vec<Diagnostic>,
) {
    let is_bool = matches!(col.data_type, DataType::Boolean);
    let is_bytea = matches!(col.data_type, DataType::Bytea);
    let is_datetime = matches!(
        col.data_type,
        DataType::Timestamp(_, _) | DataType::Date | DataType::Time(_, _)
    );
    let is_numeric = is_numeric_type(&col.data_type);
    let mut drop_default = false;
    for opt in &mut col.options {
        let ColumnOption::Default(expr) = &mut opt.option else {
            continue;
        };
        // Type-agnostic recasts first: a MySQL default literal whose *spelling*
        // is wrong in Postgres regardless of the column type.
        // `DEFAULT "hi"` parses as a double-quoted string, which Postgres reads
        // as a quoted *identifier* (`column "hi" does not exist`) — convert to a
        // single-quoted string literal.
        recast_double_quoted_string(expr);

        if is_bool {
            drop_default |= !recast_bool_default(expr);
        } else if is_bytea {
            drop_default |= !recast_bytea_default(expr, bit_width);
        } else if is_datetime {
            if let Expr::Value(v) = expr {
                if let Value::SingleQuotedString(s) = &v.value {
                    drop_default |= has_zero_date_segment(s);
                }
            }
        } else if is_numeric {
            // `DEFAULT 0x02` parses as a hex-string literal; Postgres re-emits
            // it as `X'02'`, a *bit-string* literal, type-incompatible with an
            // integer/decimal column. MySQL reads `0x..` in a numeric context
            // as its integer value — recast to a decimal number.
            drop_default |= !recast_hex_numeric_default(expr);
        }
    }
    if drop_default {
        diags.push(warn(
            LintRule::MysqlInvalidDefaultDropped,
            "DEFAULT dropped: the MySQL default is invalid for the DSQL column type.",
            format!(
                "Column `{col_name}`: the DEFAULT was removed because it cannot be represented \
                 in the translated DSQL type (e.g. a zero-date, or a non-0/1 boolean). \
                 Set an explicit default or handle it in application code."
            ),
        ));
        col.options
            .retain(|opt| !matches!(opt.option, ColumnOption::Default(_)));
    }
}

/// Recast a DEFAULT for a column rewritten to BOOLEAN. Returns false when the
/// default must be dropped (not provably a boolean value).
fn recast_bool_default(expr: &mut Expr) -> bool {
    let Expr::Value(v) = expr else { return false };
    let recast = match &v.value {
        Value::Boolean(_) | Value::Null => return true,
        Value::Number(s, _)
        | Value::SingleQuotedString(s)
        | Value::SingleQuotedByteStringLiteral(s) => match s.as_str() {
            "0" => false,
            "1" => true,
            _ => return false,
        },
        Value::HexStringLiteral(s) => match u64::from_str_radix(s, 16) {
            Ok(0) => false,
            Ok(1) => true,
            _ => return false,
        },
        _ => return false,
    };
    v.value = Value::Boolean(recast);
    true
}

/// Recast a DEFAULT for a column rewritten to BYTEA: MySQL bit literals
/// (`b'10'`) and hex literals (`0x02`) become bytea hex input (`'\x02'`) —
/// re-emitted verbatim they'd be Postgres *bit-string* literals, type-
/// incompatible with bytea. Returns false when the default must be dropped.
fn recast_bytea_default(expr: &mut Expr, bit_width: Option<u64>) -> bool {
    let Expr::Value(v) = expr else { return false };
    // Pad to the declared bit(N) width so DEFAULT-generated rows match
    // loaded rows byte-for-byte.
    let min_bytes = bit_width.map_or(1, |w| (w as usize).div_ceil(8));
    let hex = match &v.value {
        Value::Null => return true,
        Value::SingleQuotedByteStringLiteral(bits) => match bits_to_bytea_hex(bits, min_bytes) {
            Some(hex) => hex,
            None => return false,
        },
        Value::HexStringLiteral(s) => {
            if s.is_empty() || !s.bytes().all(|b| b.is_ascii_hexdigit()) {
                return false;
            }
            hex_to_bytea_literal(&s.to_lowercase(), min_bytes)
        }
        _ => return false,
    };
    v.value = Value::SingleQuotedString(hex);
    true
}

/// Assemble a Postgres bytea hex literal (`\xNN..`) from raw lowercase hex
/// digits: pad to an even digit count (whole bytes) and to at least `min_bytes`
/// bytes, MSB-first. Shared by the bit-literal and hex-literal bytea recasts.
fn hex_to_bytea_literal(hex_digits: &str, min_bytes: usize) -> String {
    let mut h = hex_digits.to_string();
    if h.len() % 2 == 1 {
        h.insert(0, '0');
    }
    while h.len() < min_bytes * 2 {
        h.insert_str(0, "00");
    }
    format!("\\x{h}")
}

/// Convert a `DEFAULT "..."` (MySQL double-quoted string) into a single-quoted
/// string literal. In MySQL `"..."` is a string; Postgres reads it as a quoted
/// identifier, so a `DEFAULT "hi"` on a varchar column fails at apply with
/// `column "hi" does not exist`. Leaves every other value shape untouched.
fn recast_double_quoted_string(expr: &mut Expr) {
    if let Expr::Value(v) = expr {
        if let Value::DoubleQuotedString(s) = &v.value {
            v.value = Value::SingleQuotedString(s.clone());
        }
    }
}

/// Recast a hex-string DEFAULT (`0x02`) on a numeric column to its decimal
/// value: Postgres re-emits `HexStringLiteral` as a bit-string `X'02'`, which
/// is type-incompatible with an integer/decimal column, whereas MySQL treats
/// `0x..` in a numeric context as the integer it encodes. Returns false when
/// the hex doesn't fit u64 (dropped + warned rather than silently wrong).
fn recast_hex_numeric_default(expr: &mut Expr) -> bool {
    let Expr::Value(v) = expr else { return true };
    let Value::HexStringLiteral(s) = &v.value else {
        return true;
    };
    match u64::from_str_radix(s, 16) {
        Ok(n) => {
            v.value = Value::Number(n.to_string(), false);
            true
        }
        Err(_) => false,
    }
}

/// Whether a (post-rewrite) Postgres type is an exact/approximate numeric type
/// — the types for which a MySQL hex-string default must become a decimal.
fn is_numeric_type(ty: &DataType) -> bool {
    matches!(
        ty,
        DataType::TinyInt(_)
            | DataType::SmallInt(_)
            | DataType::Int(_)
            | DataType::Integer(_)
            | DataType::BigInt(_)
            | DataType::Numeric(_)
            | DataType::Decimal(_)
            | DataType::Dec(_)
            | DataType::Float(_)
            | DataType::Real
            | DataType::Double(_)
            | DataType::DoublePrecision
    )
}

/// MySQL zero-in-date sentinels (`0000-00-00`, `2004-00-15`, `2004-01-00`,
/// allowed when NO_ZERO_IN_DATE is off) are out of range for every Postgres
/// date/time type.
fn has_zero_date_segment(s: &str) -> bool {
    let date: String = s.chars().take(10).collect();
    date.starts_with("0000-") || date.contains("-00-") || date.ends_with("-00")
}

/// Convert a MySQL bit-string literal body (`b'00000010'` → `"00000010"`) to
/// Postgres bytea hex input (`\x02`), MSB-first, zero-padded to at least
/// `min_bytes`. Returns None for non-binary digits.
fn bits_to_bytea_hex(bits: &str, min_bytes: usize) -> Option<String> {
    if bits.is_empty() || !bits.bytes().all(|b| b == b'0' || b == b'1') {
        return None;
    }
    // Group the bits into whole bytes (MSB-first) and render each as two hex
    // digits, then let the shared helper apply the min-byte padding.
    let width = bits.len().div_ceil(8) * 8;
    let padded = format!("{bits:0>width$}");
    let mut hex = String::new();
    for chunk in padded.as_bytes().chunks(8) {
        let byte = chunk.iter().fold(0u8, |acc, b| (acc << 1) | (b - b'0'));
        hex.push_str(&format!("{byte:02x}"));
    }
    Some(hex_to_bytea_literal(&hex, min_bytes))
}

/// Strip backticks inside a column's expression-bearing options (`DEFAULT
/// <expr>`, `GENERATED ALWAYS AS (<expr>)`) — they would re-emit and fail the
/// Postgres parse.
fn unquote_column_option_exprs(col: &mut sqlparser::ast::ColumnDef) {
    for opt in &mut col.options {
        match &mut opt.option {
            ColumnOption::Default(expr) => unquote_expr(expr),
            ColumnOption::Generated {
                generation_expr: Some(expr),
                ..
            } => unquote_expr(expr),
            _ => {}
        }
    }
}

/// Whether a column option is MySQL's `AUTO_INCREMENT`. The parser normalizes
/// it to a single canonical keyword token regardless of source case.
fn is_auto_increment(option: &ColumnOption) -> bool {
    matches!(
        option,
        ColumnOption::DialectSpecific(tokens)
            if matches!(tokens.as_slice(), [Token::Word(w)] if w.value == "AUTO_INCREMENT")
    )
}

/// Build a `CREATE INDEX <name> ON <table> (cols)` from a lifted inline
/// secondary `KEY`/`INDEX`. The existing `fix_sql` IndexAsync rule rewrites it
/// to `CREATE INDEX ASYNC` for DSQL.
///
/// MySQL index names are scoped per table; Postgres/DSQL share one schema-wide
/// index namespace. mysqldump routinely names a KEY after its column
/// (`KEY user_id (user_id)`), so the same name recurs across tables and a
/// second bare `CREATE INDEX user_id` would fail at apply ("relation already
/// exists"). Qualify the lifted name with the table (`{table}_{name}`) to keep
/// it unique, and warn — the index's name changed, which callers referencing it
/// by name (e.g. index hints, `DROP INDEX`) must know about.
fn lift_index(
    table: &ObjectName,
    idx: &mut sqlparser::ast::IndexConstraint,
    diags: &mut Vec<Diagnostic>,
) -> Statement {
    let name = idx.name.take().map(|mut n| {
        unquote_ident(&mut n);
        let table_leaf = object_name_leaf(table);
        let qualified = format!("{table_leaf}_{}", n.value);
        diags.push(warn(
            LintRule::MysqlIndexRenamed,
            "Secondary index renamed to stay unique in DSQL's schema-wide index namespace.",
            format!(
                "Index `{}` on table `{table_leaf}` was lifted to `CREATE INDEX {qualified}` \
                 (MySQL index names are per-table; DSQL's are schema-wide, so the original name \
                 could collide with an identically-named index on another table). Update any \
                 index hints or DROP INDEX statements that referenced the old name.",
                n.value
            ),
        ));
        let quote_style = if needs_double_quote(&qualified) {
            Some('"')
        } else {
            None
        };
        ObjectName(vec![ObjectNamePart::Identifier(Ident {
            value: qualified,
            quote_style,
            span: Span::empty(),
        })])
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

/// The last (unqualified) identifier of an object name — `db`.`t` → `t`. Used
/// to prefix a lifted index name with just the table, not the schema.
fn object_name_leaf(name: &ObjectName) -> String {
    name.0
        .iter()
        .rev()
        .find_map(|part| match part {
            ObjectNamePart::Identifier(id) => Some(id.value.clone()),
            _ => None,
        })
        .unwrap_or_default()
}

/// Whether a single-part object name equals `target` (ASCII case-insensitive).
fn object_name_eq_ci(name: &ObjectName, target: &str) -> bool {
    name.0.len() == 1
        && matches!(
            &name.0[0],
            ObjectNamePart::Identifier(id) if id.value.eq_ignore_ascii_case(target)
        )
}

fn num_expr(n: u64) -> Expr {
    Expr::Value(ValueWithSpan {
        value: Value::Number(n.to_string(), false),
        span: Span::empty(),
    })
}

/// Map a MySQL column data type to its DSQL (Postgres) equivalent. Types with
/// a direct Postgres spelling (varchar, text, date, time, decimal, ...) pass
/// through. Lossy arms warn; value-preserving arms stay silent so reviewers
/// aren't trained to ignore warnings.
fn normalize_data_type(ty: &mut DataType, col_name: &str, diags: &mut Vec<Diagnostic>) {
    let replacement = match ty {
        // tinyint(1) is MySQL's boolean convention; wider/!=1 → SMALLINT.
        // `bool`/`boolean` are aliases for tinyint(1) in MySQL (mysqldump
        // normalizes them away, but hand-written DDL keeps them).
        DataType::TinyInt(Some(1)) | DataType::Bool | DataType::Boolean => DataType::Boolean,
        DataType::TinyInt(_) | DataType::TinyIntUnsigned(_) => DataType::SmallInt(None),
        // No MEDIUMINT in DSQL.
        DataType::MediumInt(_) | DataType::MediumIntUnsigned(_) => DataType::Integer(None),
        // Unsigned widening: the next signed type holds the full unsigned
        // range, but DSQL gains no `CHECK (col >= 0)` — negatives MySQL forbade
        // become storable, so this is lossy.
        DataType::SmallIntUnsigned(_) => {
            diags.push(unsigned_warning(col_name, "INTEGER"));
            DataType::Integer(None)
        }
        DataType::IntUnsigned(_) | DataType::IntegerUnsigned(_) => {
            diags.push(unsigned_warning(col_name, "BIGINT"));
            DataType::BigInt(None)
        }
        // bigint unsigned overflows i64 → NUMERIC. Bare NUMERIC (not
        // NUMERIC(20,0) + CHECK) is deferred polish (L4).
        DataType::BigIntUnsigned(_) => {
            diags.push(unsigned_warning(col_name, "NUMERIC"));
            DataType::Numeric(ExactNumberInfo::None)
        }
        // Postgres has no integer type modifier; drop MySQL display widths.
        DataType::Int(Some(_)) => DataType::Int(None),
        DataType::Integer(Some(_)) => DataType::Integer(None),
        DataType::SmallInt(Some(_)) => DataType::SmallInt(None),
        DataType::BigInt(Some(_)) => DataType::BigInt(None),
        // Fractional-second precision carries over (timestamp(n) is valid PG).
        DataType::Datetime(p) => DataType::Timestamp(*p, TimezoneInfo::None),
        // YEAR parses as a custom type name; no DSQL equivalent.
        DataType::Custom(name, _) if object_name_eq_ci(name, "year") => DataType::Integer(None),
        // ENUM → VARCHAR(255): the allowed-values constraint is lost (no CHECK).
        DataType::Enum(_, _) => {
            diags.push(warn(
                LintRule::MysqlEnumToVarchar,
                "ENUM translated to VARCHAR(255) without value validation.",
                format!(
                    "Column `{col_name}`: ENUM became VARCHAR(255); the allowed-values constraint \
                     is not enforced. Add a CHECK (col IN (...)) or validate in application code."
                ),
            ));
            DataType::Varchar(Some(CharacterLength::IntegerLength {
                length: 255,
                unit: None,
            }))
        }
        // SET → TEXT: multi-membership semantics and allowed-values are lost.
        DataType::Set(_) => {
            diags.push(warn(
                LintRule::MysqlSetToText,
                "SET translated to TEXT without value validation.",
                format!(
                    "Column `{col_name}`: SET became TEXT; the allowed-values set and \
                     multi-membership semantics are not enforced. Validate in application code."
                ),
            ));
            DataType::Text
        }
        // Binary/BLOB family → BYTEA (faithful; DSQL has no BLOB/BINARY/VARBINARY).
        DataType::Blob(_)
        | DataType::TinyBlob
        | DataType::MediumBlob
        | DataType::LongBlob
        | DataType::Binary(_)
        | DataType::Varbinary(_) => DataType::Bytea,
        // bit(1) is MySQL's other boolean spelling; wider bit → BYTEA.
        DataType::Bit(Some(1)) => DataType::Boolean,
        DataType::Bit(_) | DataType::BitVarying(_) => DataType::Bytea,
        // tiny/medium/longtext have no Postgres spelling → TEXT (faithful).
        DataType::TinyText | DataType::MediumText | DataType::LongText => DataType::Text,
        // Floating point: DSQL has no MySQL `DOUBLE`/`DOUBLE(m,d)` spelling and
        // rejects the `UNSIGNED` modifier. Map to DOUBLE PRECISION, dropping the
        // (m,d) display precision (Postgres float types take no scale). The
        // signed form is faithful; the unsigned forms drop the ≥0 invariant.
        DataType::Double(_) => DataType::DoublePrecision,
        DataType::DoubleUnsigned(_) | DataType::DoublePrecisionUnsigned => {
            diags.push(unsigned_warning(col_name, "DOUBLE PRECISION"));
            DataType::DoublePrecision
        }
        DataType::Float(info) => DataType::Float(float_info(info)),
        DataType::FloatUnsigned(info) => {
            diags.push(unsigned_warning(col_name, "FLOAT"));
            DataType::Float(float_info(info))
        }
        DataType::RealUnsigned => {
            diags.push(unsigned_warning(col_name, "REAL"));
            DataType::Real
        }
        // Unsigned exact-numeric: DSQL rejects `UNSIGNED`; precision/scale carry
        // over. Lossy — the non-negative invariant is dropped (no CHECK).
        DataType::DecimalUnsigned(info) => {
            diags.push(unsigned_warning(col_name, "DECIMAL"));
            DataType::Decimal(*info)
        }
        DataType::DecUnsigned(info) => {
            diags.push(unsigned_warning(col_name, "DEC"));
            DataType::Dec(*info)
        }
        _ => return,
    };
    *ty = replacement;
}

/// Postgres accepts `FLOAT(p)` but not MySQL's `FLOAT(m,d)`; keep a lone
/// precision, drop the (m,d) display form to a bare `FLOAT`.
fn float_info(info: &ExactNumberInfo) -> ExactNumberInfo {
    match info {
        ExactNumberInfo::Precision(p) => ExactNumberInfo::Precision(*p),
        _ => ExactNumberInfo::None,
    }
}

/// Warning for an unsigned numeric column mapped to a signed DSQL type: the
/// target holds the range, but the non-negative invariant is dropped (DSQL has
/// no `UNSIGNED`, and no `CHECK (col >= 0)` is added).
fn unsigned_warning(col_name: &str, target: &str) -> Diagnostic {
    warn(
        LintRule::MysqlUnsignedWidened,
        "Unsigned numeric type mapped to a signed DSQL type.",
        format!(
            "Column `{col_name}`: an unsigned numeric type became {target}; DSQL has no UNSIGNED, \
             and no CHECK (col >= 0) is added, so negative values MySQL rejected are now storable."
        ),
    )
}

/// Strip backticks from every identifier a table constraint carries: its
/// optional constraint/index name and its column list. Warns when dropping an
/// index prefix length changes PRIMARY KEY/UNIQUE semantics: prefix uniqueness
/// and full-column uniqueness accept different row sets.
fn unquote_constraint(constraint: &mut TableConstraint, diags: &mut Vec<Diagnostic>) {
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

    // Secondary `KEY`/`INDEX` constraints (`TableConstraint::Index`) never
    // reach here — they're lifted to standalone CREATE INDEX statements before
    // this runs. The remaining name-and-column-bearing constraints are
    // PRIMARY KEY and UNIQUE, both of which enforce uniqueness, so a dropped
    // prefix always changes semantics and always warns.
    let (name, index_name, columns): (&mut Option<Ident>, &mut Option<Ident>, &mut [IndexColumn]) =
        match constraint {
            TableConstraint::PrimaryKey(c) => (&mut c.name, &mut c.index_name, &mut c.columns),
            TableConstraint::Unique(c) => (&mut c.name, &mut c.index_name, &mut c.columns),
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
                unquote_expr(&mut c.expr);
                return;
            }
            // Remaining variants (FulltextOrSpatial, *UsingIndex) carry idents
            // too, but mysqldump's default CREATE TABLE does not emit them.
            // Leave them for fix_sql to reject explicitly rather than silently
            // half-handle.
            _ => return,
        };
    unquote_opt_ident(name);
    unquote_opt_ident(index_name);
    for col in columns {
        // Dropping a prefix inside PRIMARY KEY/UNIQUE weakens the constraint:
        // values distinct beyond the first N characters were duplicates in
        // MySQL but coexist here, so future inserts MySQL would have rejected
        // now succeed.
        if unquote_index_column(col) {
            diags.push(warn(
                LintRule::MysqlIndexPrefixDropped,
                "Index prefix length dropped inside a PRIMARY KEY/UNIQUE constraint.",
                "A `col(N)` prefix in a unique constraint was replaced by the full column: \
                 MySQL enforced uniqueness on the first N characters, DSQL enforces it on the \
                 whole value. Existing data reloads fine, but inserts MySQL would have rejected \
                 as prefix duplicates now succeed — add application-side checks if callers \
                 depend on the stricter rule."
                    .to_string(),
            ));
        }
    }
}

/// Returns true if a MySQL index prefix length was dropped.
fn unquote_index_column(col: &mut IndexColumn) -> bool {
    // A MySQL index prefix length — `KEY k (name(20))` — parses as a Function
    // call. Postgres has no prefix indexes; index the whole column instead
    // (same query semantics, more storage). Only a single numeric "argument"
    // is a prefix length, so nothing else matches this shape.
    let mut dropped_prefix = false;
    if let Expr::Function(f) = &col.column.expr {
        if let (
            [ObjectNamePart::Identifier(ident)],
            Some([FunctionArg::Unnamed(FunctionArgExpr::Expr(Expr::Value(v)))]),
        ) = (
            f.name.0.as_slice(),
            match &f.args {
                FunctionArguments::List(list) => Some(list.args.as_slice()),
                _ => None,
            },
        ) {
            if matches!(v.value, Value::Number(_, _)) {
                col.column.expr = Expr::Identifier(ident.clone());
                dropped_prefix = true;
            }
        }
    }
    unquote_expr(&mut col.column.expr);
    dropped_prefix
}

/// Strip backticks from every identifier in an expression (CHECK predicates,
/// indexed-column expressions, function arguments, …). Walks the whole `Expr`
/// tree via sqlparser's visitor, so a backtick buried in any expression shape
/// (`json_valid(`j`)`, casts, subscripts) is reached — a hand-rolled match
/// covered only a handful of variants and left the rest to fail the parse.
fn unquote_expr(expr: &mut Expr) {
    let _: ControlFlow<()> = visit_expressions_mut(expr, |e| {
        match e {
            Expr::Identifier(ident) => unquote_ident(ident),
            Expr::CompoundIdentifier(parts) => parts.iter_mut().for_each(unquote_ident),
            _ => {}
        }
        ControlFlow::Continue(())
    });
}

fn unquote_opt_ident(ident: &mut Option<Ident>) {
    if let Some(ident) = ident {
        unquote_ident(ident);
    }
}

fn unquote_object_name(name: &mut ObjectName) {
    for part in &mut name.0 {
        if let ObjectNamePart::Identifier(ident) = part {
            unquote_ident(ident);
        }
    }
}

/// Convert a backtick-quoted MySQL identifier to its DSQL form. Postgres folds
/// unquoted identifiers to lower case and rejects reserved words as bare names,
/// so dropping the backtick is only safe when the name is already all-lowercase
/// and not reserved. Otherwise re-quote as `"..."`: `` `Order` `` → `"Order"`,
/// `` `user` `` → `"user"`. A bare `order`/`Order`/`Users` would either fail the
/// parse or silently fold to a different name that no longer matches the source.
fn unquote_ident(ident: &mut Ident) {
    if ident.quote_style != Some('`') {
        return;
    }
    if needs_double_quote(&ident.value) {
        ident.quote_style = Some('"');
    } else {
        ident.quote_style = None;
    }
}

/// Whether an identifier must be double-quoted to survive as-is in Postgres:
/// it has an uppercase letter (bare form would fold to lowercase, changing the
/// name) or it collides with a Postgres reserved word.
fn needs_double_quote(value: &str) -> bool {
    value.chars().any(|c| c.is_ascii_uppercase()) || is_pg_reserved_word(value)
}

/// Postgres reserved keywords that cannot appear as a bare column or table
/// name. Covers BOTH reserved categories from the PostgreSQL keyword appendix:
/// plain "reserved" AND "reserved (can be function or type name)" — the latter
/// may name a function/type but is still rejected as a bare column/table name,
/// so it must be quoted too. sqlparser's own keyword lists don't model
/// Postgres's reserved/non-reserved split, so this is the explicit set.
/// Non-reserved words (`name`, `value`, `type`, …) are deliberately absent so
/// the common case stays unquoted. Matched case-insensitively.
fn is_pg_reserved_word(value: &str) -> bool {
    const RESERVED: &[&str] = &[
        // "reserved"
        "all", "analyse", "analyze", "and", "any", "array", "as", "asc", "asymmetric", "both",
        "case", "cast", "check", "collate", "column", "constraint", "create", "current_catalog",
        "current_date", "current_role", "current_time", "current_timestamp", "current_user",
        "default", "deferrable", "desc", "distinct", "do", "else", "end", "except", "false",
        "fetch", "for", "foreign", "from", "grant", "group", "having", "in", "initially",
        "intersect", "into", "lateral", "leading", "limit", "localtime", "localtimestamp", "not",
        "null", "offset", "on", "only", "or", "order", "placing", "primary", "references",
        "returning", "select", "session_user", "some", "symmetric", "system_user", "table",
        "then", "to", "trailing", "true", "union", "unique", "user", "using", "variadic", "when",
        "where", "window", "with",
        // "reserved (can be function or type name)" — still invalid as a bare
        // column/table name.
        "authorization", "binary", "collation", "concurrently", "cross", "current_schema",
        "freeze", "full", "ilike", "inner", "is", "isnull", "join", "left", "like", "natural",
        "notnull", "outer", "overlaps", "right", "similar", "tablesample", "verbose",
    ];
    RESERVED.contains(&value.to_ascii_lowercase().as_str())
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
            "BLOB",
            "VARBINARY",
            "BINARY",
            "TINYTEXT",
            "MEDIUMTEXT",
            "LONGTEXT",
        ] {
            assert!(
                !u.contains(banned),
                "MySQL-ism {banned:?} survived into output (lenient PG parser won't flag it):\n{}",
                out.sql
            );
        }
    }

    /// True if any diagnostic is a `FixedWithWarning` whose detail contains
    /// `needle` (case-insensitive) — the honest signal that a lossy transform
    /// changed the data's meaning.
    fn has_warning(out: &FixOutput, needle: &str) -> bool {
        out.diagnostics.iter().any(|d| {
            matches!(&d.fix_result, crate::FixResult::FixedWithWarning(s)
                if s.to_lowercase().contains(&needle.to_lowercase()))
        })
    }

    /// A lossy transform must NOT be silent — each surfaces a FixedWithWarning
    /// so `FIXED` keeps meaning "semantically faithful". `enum`/`set` drop value
    /// validation, unsigned widening drops the non-negative invariant, a dropped
    /// `ON UPDATE` loses auto-update.
    #[test]
    fn lossy_transforms_emit_warnings() {
        assert!(
            has_warning(
                &fix_sql_mysql("CREATE TABLE `t` (`k` enum('a','b'));"),
                "enum"
            ),
            "enum->VARCHAR must warn (value validation dropped)"
        );
        assert!(
            has_warning(
                &fix_sql_mysql("CREATE TABLE `t` (`s` set('r','w'));"),
                "set"
            ),
            "set->TEXT must warn (value validation dropped)"
        );
        assert!(
            has_warning(
                &fix_sql_mysql("CREATE TABLE `t` (`x` bigint unsigned);"),
                "unsigned"
            ),
            "bigint unsigned->NUMERIC must warn (range guard dropped)"
        );
        assert!(
            has_warning(
                &fix_sql_mysql("CREATE TABLE `t` (`x` int unsigned);"),
                "unsigned"
            ),
            "int unsigned->BIGINT must warn (non-negative invariant dropped)"
        );
        assert!(
            has_warning(
                &fix_sql_mysql(
                    "CREATE TABLE `t` (`u` timestamp DEFAULT CURRENT_TIMESTAMP ON UPDATE CURRENT_TIMESTAMP);"
                ),
                "on update"
            ),
            "dropped ON UPDATE must warn (auto-update lost)"
        );
        assert!(
            has_warning(
                &fix_sql_mysql(
                    "CREATE TABLE `t` (`id` int NOT NULL AUTO_INCREMENT, PRIMARY KEY (`id`));"
                ),
                "identity"
            ),
            "AUTO_INCREMENT->IDENTITY must warn (sequence not seeded)"
        );
    }

    /// A faithful transform preserves the value and must stay silent — no
    /// spurious warning that would train reviewers to ignore them.
    #[test]
    fn faithful_transforms_do_not_warn() {
        let out = fix_sql_mysql(
            "CREATE TABLE `t` (`d` datetime, `b` tinyint(1), `n` int(11), `x` blob);",
        );
        assert!(
            !out.diagnostics
                .iter()
                .any(|d| matches!(d.fix_result, crate::FixResult::FixedWithWarning(_))),
            "value-preserving transforms must not warn, got: {:?}",
            out.diagnostics
        );
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

    /// MySQL DATETIME has no Postgres equivalent name → TIMESTAMP; the
    /// fractional-second precision carries over.
    #[test]
    fn maps_datetime_to_timestamp() {
        let sql = "CREATE TABLE `t` (`created` datetime, `precise` datetime(6)) ENGINE=InnoDB;";
        let out = fix_sql_mysql(sql);
        assert_clean_dsql(&out);
        let u = out.sql.to_uppercase();
        assert!(
            u.contains("CREATED TIMESTAMP"),
            "datetime->TIMESTAMP, got:\n{}",
            out.sql
        );
        assert!(
            u.contains("PRECISE TIMESTAMP(6)"),
            "datetime(6)->TIMESTAMP(6), got:\n{}",
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

    /// FOREIGN KEY backticks (constraint name, FK columns, referenced table and
    /// columns) must all be stripped so they never reach the Postgres parser.
    /// The FK itself is removed by the existing fix_sql ForeignKey rule.
    #[test]
    fn unquotes_foreign_key_backticks() {
        let sql = "CREATE TABLE `t` (`id` int, `cid` int, \
                   CONSTRAINT `fk_c` FOREIGN KEY (`cid`) REFERENCES `other` (`id`));";
        let out = fix_sql_mysql(sql);
        assert_clean_dsql(&out);
    }

    /// A backtick-quoted CHECK constraint name must be unquoted.
    #[test]
    fn unquotes_check_constraint_name() {
        let sql = "CREATE TABLE `t` (`id` int, CONSTRAINT `ck` CHECK (`id` > 0));";
        let out = fix_sql_mysql(sql);
        assert_clean_dsql(&out);
        assert!(
            out.sql.to_uppercase().contains("CHECK"),
            "CHECK must survive, got:\n{}",
            out.sql
        );
    }

    /// The small unsigned variants each widen to the next signed type.
    #[test]
    fn widens_unsigned_small_integer_variants() {
        let sql =
            "CREATE TABLE `t` (`a` tinyint unsigned, `b` smallint unsigned, `c` mediumint unsigned);";
        let out = fix_sql_mysql(sql);
        assert_clean_dsql(&out);
        let u = out.sql.to_uppercase();
        assert!(
            u.contains("A SMALLINT"),
            "tinyint unsigned->SMALLINT:\n{}",
            out.sql
        );
        assert!(
            u.contains("B INTEGER"),
            "smallint unsigned->INTEGER:\n{}",
            out.sql
        );
        assert!(
            u.contains("C INTEGER"),
            "mediumint unsigned->INTEGER:\n{}",
            out.sql
        );
    }

    /// Unsigned widening and display-width dropping compose on one column.
    #[test]
    fn widens_unsigned_with_display_width() {
        let out = fix_sql_mysql("CREATE TABLE `t` (`x` int(11) unsigned);");
        assert_clean_dsql(&out);
        assert!(
            out.sql.to_uppercase().contains("X BIGINT"),
            "int(11) unsigned->BIGINT:\n{}",
            out.sql
        );
    }

    /// A session `SET` is dropped but a following CREATE TABLE is kept.
    #[test]
    fn drops_session_set_keeps_create_table() {
        let out = fix_sql_mysql("SET NAMES utf8mb4; CREATE TABLE t (id INT);");
        let u = out.sql.to_uppercase();
        assert!(
            u.contains("CREATE TABLE"),
            "CREATE TABLE kept:\n{}",
            out.sql
        );
        assert!(!u.contains("SET NAMES"), "SET dropped:\n{}", out.sql);
    }

    /// `mysqldump --databases` emits `USE dbname;` (bare or backticked) — it
    /// must be dropped as noise, not re-emitted for the Postgres gate to
    /// reject as a ParseError.
    #[test]
    fn drops_use_statement() {
        for sql in [
            "USE `mydb`;\nCREATE TABLE `t` (`id` int);",
            "USE mydb;\nCREATE TABLE `t` (`id` int);",
        ] {
            let out = fix_sql_mysql(sql);
            assert_clean_dsql(&out);
            let u = out.sql.to_uppercase();
            assert!(u.contains("CREATE TABLE"), "table kept:\n{}", out.sql);
            assert!(!u.contains("USE "), "USE dropped:\n{}", out.sql);
        }
    }

    /// Input that is only MySQL-only noise yields empty output, no diagnostics.
    #[test]
    fn noise_only_input_yields_empty_output() {
        let out = fix_sql_mysql("LOCK TABLES t WRITE; UNLOCK TABLES;");
        assert!(out.sql.trim().is_empty(), "expected empty:\n{}", out.sql);
        assert!(
            !out.diagnostics
                .iter()
                .any(|d| matches!(d.rule, crate::LintRule::ParseError)),
            "no ParseError on noise-only input: {:?}",
            out.diagnostics
        );
    }

    /// Empty input round-trips to empty output with no diagnostics.
    #[test]
    fn empty_input_yields_empty_output() {
        let out = fix_sql_mysql("");
        assert!(out.sql.is_empty());
        assert!(out.diagnostics.is_empty());
    }

    /// A MySQL index prefix length (`KEY k (name(20))`) has no Postgres form —
    /// the prefix is dropped and the whole column indexed. On a plain KEY
    /// that's a storage detail (silent); inside UNIQUE/PRIMARY KEY it changes
    /// which rows collide, so it must warn.
    #[test]
    fn drops_index_prefix_length() {
        let out = fix_sql_mysql(
            "CREATE TABLE `t` (`id` int NOT NULL, `name` varchar(200), \
             PRIMARY KEY (`id`), KEY `idx_name` (`name`(20)));",
        );
        assert_clean_dsql(&out);
        let u = out.sql.to_uppercase();
        assert!(
            u.contains("ON T(NAME)"),
            "prefix length dropped, whole column indexed:\n{}",
            out.sql
        );
        assert!(!u.contains("(20)"), "no prefix survives:\n{}", out.sql);
        assert!(
            !has_warning(&out, "prefix"),
            "plain KEY prefix drop is silent:\n{:?}",
            out.diagnostics
        );

        let out = fix_sql_mysql(
            "CREATE TABLE `t` (`id` int NOT NULL, `name` varchar(200), \
             PRIMARY KEY (`id`), UNIQUE KEY `uk` (`name`(20)));",
        );
        assert_clean_dsql(&out);
        assert!(
            !out.sql.contains("(20)"),
            "no prefix survives:\n{}",
            out.sql
        );
        assert!(
            has_warning(&out, "prefix"),
            "UNIQUE prefix drop must warn (constraint semantics change):\n{:?}",
            out.diagnostics
        );
    }

    /// After a type rewrite the MySQL DEFAULT literal may be invalid for the
    /// new Postgres type — Postgres validates at CREATE time, MySQL doesn't.
    /// 0/1 (bare, quoted, or bit) recast to FALSE/TRUE for BOOLEAN; a bit
    /// literal becomes bytea hex for BYTEA; zero-dates are dropped + warned.
    #[test]
    fn recasts_defaults_for_rewritten_types() {
        let out = fix_sql_mysql(
            "CREATE TABLE `t` (\
             `a` tinyint(1) DEFAULT 0, `b` tinyint(1) NOT NULL DEFAULT '1', \
             `f` bit(1) DEFAULT b'1', `m` bit(8) DEFAULT b'00000010', \
             `d` datetime NOT NULL DEFAULT '0000-00-00 00:00:00', \
             `s` varchar(20) DEFAULT '0000-00-00');",
        );
        assert_clean_dsql(&out);
        let u = out.sql.to_uppercase();
        assert!(
            u.contains("A BOOLEAN DEFAULT FALSE"),
            "0->FALSE:\n{}",
            out.sql
        );
        assert!(
            u.contains("B BOOLEAN NOT NULL DEFAULT TRUE"),
            "'1'->TRUE:\n{}",
            out.sql
        );
        assert!(
            u.contains("F BOOLEAN DEFAULT TRUE"),
            "b'1'->TRUE:\n{}",
            out.sql
        );
        assert!(
            out.sql.contains("m BYTEA DEFAULT '\\x02'"),
            "b'00000010'->bytea hex:\n{}",
            out.sql
        );
        assert!(
            u.contains("D TIMESTAMP NOT NULL") && !u.contains("0000-00-00 00:00:00"),
            "zero-date default dropped:\n{}",
            out.sql
        );
        assert!(
            has_warning(&out, "default was removed"),
            "zero-date drop warns:\n{:?}",
            out.diagnostics
        );
        assert!(
            out.sql.contains("s VARCHAR(20) DEFAULT '0000-00-00'"),
            "a string column's zero-date-looking default is untouched:\n{}",
            out.sql
        );
    }

    /// Default shapes we can't prove valid for the rewritten type — `DEFAULT
    /// -1` (UnaryOp) or `DEFAULT (0)` (Nested) on BOOLEAN, a bare number on
    /// BYTEA — are dropped + warned, not emitted as invalid DSQL the lenient
    /// PG gate can't catch. Hex literals (0x01) recast like bit literals.
    /// MySQL's `bool` alias and partial zero-dates (`2004-01-00`) work too.
    #[test]
    fn unprovable_defaults_dropped_not_emitted() {
        let out = fix_sql_mysql(
            "CREATE TABLE `t` (\
             `a` tinyint(1) DEFAULT -1, `b` tinyint(1) DEFAULT (0), \
             `c` bit(8) DEFAULT 2, `d` tinyint(1) DEFAULT 0x01, \
             `e` bit(8) DEFAULT 0x02, `f` bool DEFAULT 0, \
             `g` date DEFAULT '2004-01-00', `w` bit(16) DEFAULT b'101');",
        );
        assert_clean_dsql(&out);
        let u = out.sql.to_uppercase();
        for gone in [
            "DEFAULT -1",
            "DEFAULT (0)",
            "C BYTEA DEFAULT 2",
            "'2004-01-00'",
        ] {
            assert!(
                !u.contains(gone),
                "unprovable default {gone:?} must be dropped:\n{}",
                out.sql
            );
        }
        assert!(
            u.contains("D BOOLEAN DEFAULT TRUE"),
            "0x01->TRUE:\n{}",
            out.sql
        );
        assert!(
            out.sql.contains("e BYTEA DEFAULT '\\x02'"),
            "0x02->bytea hex:\n{}",
            out.sql
        );
        assert!(
            u.contains("F BOOLEAN DEFAULT FALSE"),
            "bool alias recasts like tinyint(1):\n{}",
            out.sql
        );
        assert!(
            out.sql.contains("w BYTEA DEFAULT '\\x0005'"),
            "b'101' on bit(16) pads to the declared width:\n{}",
            out.sql
        );
        assert_eq!(
            out.diagnostics
                .iter()
                .filter(|d| matches!(d.rule, crate::LintRule::MysqlInvalidDefaultDropped))
                .count(),
            4,
            "each dropped default warns once: {:?}",
            out.diagnostics
        );
    }

    #[test]
    fn bits_to_bytea_hex_edges() {
        assert_eq!(bits_to_bytea_hex("00000010", 1).as_deref(), Some("\\x02"));
        assert_eq!(bits_to_bytea_hex("1", 1).as_deref(), Some("\\x01"));
        assert_eq!(bits_to_bytea_hex("101", 2).as_deref(), Some("\\x0005"));
        assert_eq!(
            bits_to_bytea_hex("1000000000000001", 1).as_deref(),
            Some("\\x8001")
        );
        assert_eq!(bits_to_bytea_hex("", 1), None);
        assert_eq!(bits_to_bytea_hex("102", 1), None);
    }

    /// Diagnostics point at the statement's line in the original MySQL file,
    /// not line 0 or a line in the internal re-emitted buffer. Covers all
    /// three producers: translation warnings, gate warnings on translated
    /// statements (ASYNC index), and gate ParseErrors on forwarded
    /// untranslatable tables.
    #[test]
    fn warnings_carry_source_line_numbers() {
        let out = fix_sql_mysql(
            "DROP TABLE IF EXISTS `t`;\n\n\
             CREATE TABLE `t` (`k` enum('a','b'), KEY `i` (`k`));\n\n\
             CREATE TABLE `u` (`x` int unsigned);\n\n\
             CREATE TABLE zf (c int zerofill);\n",
        );
        let line_of = |rule: fn(&crate::LintRule) -> bool| {
            out.diagnostics
                .iter()
                .find(|d| rule(&d.rule))
                .map(|d| d.line)
        };
        assert_eq!(
            line_of(|r| matches!(r, crate::LintRule::MysqlEnumToVarchar)),
            Some(3),
            "translation warning: {:?}",
            out.diagnostics
        );
        assert_eq!(
            line_of(|r| matches!(r, crate::LintRule::IndexAsync)),
            Some(3),
            "gate warning remapped to the CREATE TABLE's source line: {:?}",
            out.diagnostics
        );
        assert_eq!(
            line_of(|r| matches!(r, crate::LintRule::MysqlUnsignedWidened)),
            Some(5),
            "translation warning: {:?}",
            out.diagnostics
        );
        assert_eq!(
            line_of(|r| matches!(r, crate::LintRule::ParseError)),
            Some(7),
            "forwarded table's ParseError remapped to its source line: {:?}",
            out.diagnostics
        );
    }

    /// An anonymous secondary `KEY (col)` lifts to an unnamed CREATE INDEX
    /// (DSQL auto-names it).
    #[test]
    fn lifts_anonymous_secondary_key() {
        let out =
            fix_sql_mysql("CREATE TABLE t (id INT PRIMARY KEY, name VARCHAR(50), KEY (name));");
        assert_clean_dsql(&out);
        let u = out.sql.to_uppercase();
        assert!(
            u.contains("CREATE INDEX"),
            "anonymous KEY lifted:\n{}",
            out.sql
        );
        assert!(
            u.contains("ON T(NAME)"),
            "index references t(name):\n{}",
            out.sql
        );
    }

    /// Backticks in a composite PRIMARY KEY column list are all stripped.
    #[test]
    fn composite_primary_key_unquoted() {
        let out = fix_sql_mysql(
            "CREATE TABLE `t` (`a` int NOT NULL, `b` int NOT NULL, PRIMARY KEY (`a`, `b`));",
        );
        assert_clean_dsql(&out);
        assert!(
            out.sql.to_uppercase().contains("PRIMARY KEY (A, B)"),
            "composite PK columns unquoted:\n{}",
            out.sql
        );
    }

    /// A db-qualified backtick table name (`db`.`t`) is unquoted in both
    /// CREATE TABLE and DROP TABLE.
    #[test]
    fn unquotes_db_qualified_table_name() {
        let out = fix_sql_mysql("CREATE TABLE `db`.`t` (id int); DROP TABLE `db`.`t`;");
        assert!(!out.sql.contains('`'), "backticks gone:\n{}", out.sql);
        assert!(
            !out.diagnostics
                .iter()
                .any(|d| matches!(d.rule, crate::LintRule::ParseError)),
            "no ParseError: {:?}",
            out.diagnostics
        );
    }

    /// Multiple CREATE TABLEs in one input are each translated.
    #[test]
    fn multiple_create_tables_each_translated() {
        let out =
            fix_sql_mysql("CREATE TABLE `t1` (`id` int); CREATE TABLE `t2` (`id` int, `ref` int);");
        assert_clean_dsql(&out);
        assert_eq!(
            out.sql.to_uppercase().matches("CREATE TABLE").count(),
            2,
            "both tables translated:\n{}",
            out.sql
        );
    }

    /// Binary/BLOB family → BYTEA, bit(1) → BOOLEAN. DSQL has no BLOB/BINARY/
    /// VARBINARY/BIT — a real cluster rejects `BLOB` (caught by the cluster
    /// test's binary-types probe).
    #[test]
    fn maps_binary_and_bit_types() {
        let sql = "CREATE TABLE `t` (`d` blob, `b` binary(16), `vb` varbinary(255), \
                   `flag` bit(1), `mask` bit(8));";
        let out = fix_sql_mysql(sql);
        assert_clean_dsql(&out);
        let u = out.sql.to_uppercase();
        assert_eq!(
            u.matches("BYTEA").count(),
            4,
            "blob/binary/varbinary/bit(8)->BYTEA:\n{}",
            out.sql
        );
        assert!(u.contains("FLAG BOOLEAN"), "bit(1)->BOOLEAN:\n{}", out.sql);
    }

    /// Float family: `double`/`double(m,d)` → DOUBLE PRECISION, `float(m,d)` →
    /// bare FLOAT, `*text` → TEXT. The lenient PG parser accepts MySQL
    /// `DOUBLE`/`FLOAT(m,d)`/`LONGTEXT` verbatim, so a missing arm passes the
    /// parse but fails on a real cluster.
    #[test]
    fn maps_float_and_text_families() {
        let sql = "CREATE TABLE `t` (`a` double, `b` double(10,2), `c` float(10,2), \
                   `d` longtext, `e` mediumtext, `f` tinytext);";
        let out = fix_sql_mysql(sql);
        assert_clean_dsql(&out);
        let u = out.sql.to_uppercase();
        assert_eq!(
            u.matches("DOUBLE PRECISION").count(),
            2,
            "double/double(m,d)->DOUBLE PRECISION:\n{}",
            out.sql
        );
        assert!(
            !u.contains("(10,2)"),
            "float display (m,d) dropped:\n{}",
            out.sql
        );
        assert_eq!(u.matches(" TEXT").count(), 3, "*text->TEXT:\n{}", out.sql);
    }

    /// Unsigned exact-numeric (`decimal(m,d) unsigned`) and `double unsigned`
    /// drop the `UNSIGNED` DSQL rejects and warn the non-negative invariant is
    /// lost.
    #[test]
    fn drops_unsigned_on_decimal_and_double() {
        let out =
            fix_sql_mysql("CREATE TABLE `t` (`a` decimal(10,2) unsigned, `b` double unsigned);");
        assert_clean_dsql(&out);
        assert!(
            has_warning(&out, "no UNSIGNED"),
            "unsigned decimal/double warns:\n{:?}",
            out.diagnostics
        );
    }

    /// A `CREATE TABLE` the MySQL dialect can't parse (e.g. `int zerofill`) must
    /// NOT vanish silently — it is forwarded so fix_sql reports a ParseError.
    #[test]
    fn unparseable_create_table_surfaces_parse_error() {
        let out = fix_sql_mysql("CREATE TABLE zf (c int zerofill);");
        assert!(
            out.diagnostics
                .iter()
                .any(|d| matches!(d.rule, crate::LintRule::ParseError)),
            "unparseable CREATE TABLE must surface a ParseError, not vanish:\n{}\n{:?}",
            out.sql,
            out.diagnostics
        );
    }

    /// A good table beside an unparseable one still translates; the bad one is
    /// reported, not dropped along with the good output.
    #[test]
    fn unparseable_table_does_not_drop_sibling() {
        let out = fix_sql_mysql("CREATE TABLE good (id int); CREATE TABLE zf (c int zerofill);");
        assert!(
            out.sql.to_uppercase().contains("GOOD"),
            "good table survives:\n{}",
            out.sql
        );
        assert!(
            out.diagnostics
                .iter()
                .any(|d| matches!(d.rule, crate::LintRule::ParseError)),
            "bad table still reported:\n{:?}",
            out.diagnostics
        );
    }

    /// mysqldump `ALTER ... DISABLE KEYS` noise fails to parse but is not a
    /// CREATE TABLE, so the parse-failure forwarding must leave it dropped, not
    /// resurrect it as a spurious ParseError.
    #[test]
    fn disable_keys_noise_stays_dropped() {
        let sql = "CREATE TABLE `t` (`id` int, PRIMARY KEY (`id`)) ENGINE=InnoDB;\n\
                   /*!40000 ALTER TABLE `t` DISABLE KEYS */;";
        let out = fix_sql_mysql(sql);
        assert!(
            !out.diagnostics
                .iter()
                .any(|d| matches!(d.rule, crate::LintRule::ParseError)),
            "DISABLE KEYS noise must not produce a ParseError:\n{}\n{:?}",
            out.sql,
            out.diagnostics
        );
    }

    /// A backtick identifier inside a generated-column expression is unquoted,
    /// not re-emitted with backticks (which the PG parser rejects).
    #[test]
    fn unquotes_generated_column_expr() {
        let out = fix_sql_mysql(
            "CREATE TABLE `t` (`a` int, `b` int GENERATED ALWAYS AS (`a` + 1) STORED);",
        );
        assert_clean_dsql(&out);
    }

    /// AUTO_INCREMENT with an explicit DEFAULT drops the DEFAULT: a column can't
    /// carry both DEFAULT and GENERATED AS IDENTITY.
    #[test]
    fn auto_increment_drops_conflicting_default() {
        let out = fix_sql_mysql(
            "CREATE TABLE `t` (`id` int NOT NULL DEFAULT 7 AUTO_INCREMENT, PRIMARY KEY (`id`));",
        );
        assert_clean_dsql(&out);
        let u = out.sql.to_uppercase();
        assert!(
            u.contains("GENERATED BY DEFAULT AS IDENTITY"),
            "identity present:\n{}",
            out.sql
        );
        assert!(
            !u.contains("DEFAULT 7"),
            "conflicting DEFAULT dropped:\n{}",
            out.sql
        );
    }

    /// A `bigint unsigned AUTO_INCREMENT` PK becomes a BIGINT identity, so it
    /// must NOT also carry the unsigned-widened "became NUMERIC" warning that
    /// would contradict the actual BIGINT output.
    #[test]
    fn auto_increment_suppresses_unsigned_widen_warning() {
        let out = fix_sql_mysql(
            "CREATE TABLE `t` (`id` bigint unsigned NOT NULL AUTO_INCREMENT, PRIMARY KEY (`id`));",
        );
        assert_clean_dsql(&out);
        assert!(
            out.sql
                .to_uppercase()
                .contains("GENERATED BY DEFAULT AS IDENTITY"),
            "identity present:\n{}",
            out.sql
        );
        assert!(
            !has_warning(&out, "no UNSIGNED"),
            "AUTO_INCREMENT column must not emit a contradictory unsigned-widen warning:\n{:?}",
            out.diagnostics
        );
        assert!(
            has_warning(&out, "identity"),
            "identity warning still present:\n{:?}",
            out.diagnostics
        );
    }

    // ─── Follow-up regression tests (PR #90 review bugs) ──────────────────

    /// Bug 1: a `''`-escaped apostrophe in a DEFAULT must survive verbatim.
    /// The old token-rebuild splitter re-emitted `'it's'` (unterminated), which
    /// degraded the whole table to a ParseError. Slicing from source keeps the
    /// original bytes so the AST re-escapes correctly.
    #[test]
    fn default_with_escaped_quote_survives() {
        let out = fix_sql_mysql("CREATE TABLE `t` (`c` varchar(20) DEFAULT 'it''s');");
        assert_clean_dsql(&out);
        assert!(
            out.sql.contains("'it''s'"),
            "escaped apostrophe must round-trip:\n{}",
            out.sql
        );
    }

    /// Bug 1: mysqldump escapes backslashes (`'C:\\data\\new'`, value
    /// `C:\data\new`). The old token-rebuild splitter double-unescaped it into
    /// `C:data<newline>ew`; slicing from source hands the byte-exact literal to
    /// the parser, which re-emits the correct value.
    #[test]
    fn default_with_backslash_survives() {
        let out = fix_sql_mysql(r"CREATE TABLE `t` (`c` varchar(50) DEFAULT 'C:\\data\\new');");
        assert_clean_dsql(&out);
        assert!(
            out.sql.contains(r"'C:\data\new'"),
            "escaped backslashes must decode to single literal backslashes:\n{}",
            out.sql
        );
    }

    /// Bug 2: one statement that fails to tokenize/parse must NOT disable the
    /// gate for its siblings. A bad forwarded table surfaces a ParseError while
    /// the following good table is still fully translated (index lifted, etc.).
    #[test]
    fn one_bad_statement_does_not_disable_gate() {
        let out = fix_sql_mysql(
            "CREATE TABLE zf (c int zerofill);\n\
             CREATE TABLE `b` (`id` int, `v` int, PRIMARY KEY (`id`), KEY `k` (`v`));",
        );
        assert!(
            out.diagnostics
                .iter()
                .any(|d| matches!(d.rule, crate::LintRule::ParseError)),
            "the unparseable table must still report a ParseError:\n{:?}",
            out.diagnostics
        );
        let u = out.sql.to_uppercase();
        assert!(
            u.contains("CREATE INDEX ASYNC"),
            "the good sibling's index must still be lifted + ASYNC'd:\n{}",
            out.sql
        );
        assert!(!out.sql.contains('`'), "no backticks survive:\n{}", out.sql);
    }

    /// Bug 3: reserved-word and mixed-case identifiers must be re-quoted as
    /// `"..."`, not emitted bare. A bare `order`/`user`/`group` fails the parse;
    /// a bare `Users` folds to lowercase `users` and no longer matches the source
    /// table name.
    #[test]
    fn reserved_and_mixed_case_identifiers_requoted() {
        let out = fix_sql_mysql(
            "CREATE TABLE `order` (`user` varchar(50), `group` int);\n\
             CREATE TABLE `Users` (`id` int);",
        );
        assert_clean_dsql(&out);
        for quoted in [r#""order""#, r#""user""#, r#""group""#, r#""Users""#] {
            assert!(
                out.sql.contains(quoted),
                "{quoted} must be double-quoted, got:\n{}",
                out.sql
            );
        }
    }

    /// Bug 3: an all-lowercase, non-reserved identifier must stay bare — over-
    /// quoting common names like `name`/`value` would be needless churn.
    #[test]
    fn ordinary_identifiers_stay_bare() {
        let out = fix_sql_mysql("CREATE TABLE `t` (`name` varchar(50), `value` int);");
        assert_clean_dsql(&out);
        assert!(
            !out.sql.contains('"'),
            "ordinary lowercase names must not be quoted:\n{}",
            out.sql
        );
    }

    /// Bug 3: the "reserved (can be function or type name)" category is still
    /// invalid as a bare column/table name and must be quoted. These are common
    /// MySQL column names the lenient PG gate would accept bare but a real DSQL
    /// cluster rejects, so a no-ParseError check can't catch the miss.
    #[test]
    fn function_or_type_name_reserved_words_requoted() {
        // `binary` is also exercised here but omitted from assert_clean_dsql's
        // scope: its banned-substring probe would false-match the quoted column
        // name `"binary"` (a valid identifier, not the MySQL BINARY type).
        let out = fix_sql_mysql(
            "CREATE TABLE `t` (`left` int, `right` int, `join` int, `is` int, \
             `natural` int, `full` int);",
        );
        assert_clean_dsql(&out);
        for quoted in [
            r#""left""#,
            r#""right""#,
            r#""join""#,
            r#""is""#,
            r#""natural""#,
            r#""full""#,
        ] {
            assert!(
                out.sql.contains(quoted),
                "{quoted} (reserved, function/type-name category) must be quoted, got:\n{}",
                out.sql
            );
        }
        // `binary` as a column name must be quoted (checked directly, since the
        // clean-DSQL probe can't distinguish it from the BINARY type keyword).
        let out2 = fix_sql_mysql("CREATE TABLE `t` (`binary` int);");
        assert!(
            out2.sql.contains(r#""binary""#),
            "`binary` column must be double-quoted, got:\n{}",
            out2.sql
        );
        assert!(
            !out2.diagnostics
                .iter()
                .any(|d| matches!(d.rule, crate::LintRule::ParseError)),
            "quoted `binary` column must parse cleanly:\n{:?}",
            out2.diagnostics
        );
    }

    /// Bug 4: lifted secondary-index names are qualified with the table so two
    /// tables with the same MySQL index name (`user_id`) don't collide in DSQL's
    /// schema-wide index namespace. The rename is surfaced as a warning.
    #[test]
    fn lifted_index_names_qualified_and_warned() {
        let out = fix_sql_mysql(
            "CREATE TABLE `t1` (`user_id` int, KEY `user_id` (`user_id`));\n\
             CREATE TABLE `t2` (`user_id` int, KEY `user_id` (`user_id`));",
        );
        assert_clean_dsql(&out);
        let u = out.sql.to_uppercase();
        assert!(
            u.contains("T1_USER_ID") && u.contains("T2_USER_ID"),
            "index names must be table-qualified:\n{}",
            out.sql
        );
        assert_eq!(
            out.diagnostics
                .iter()
                .filter(|d| matches!(d.rule, crate::LintRule::MysqlIndexRenamed))
                .count(),
            2,
            "each rename must warn:\n{:?}",
            out.diagnostics
        );
    }

    /// Bug 5: mysqldump prefixes every table with a `-- Table structure ...`
    /// comment. An unparseable CREATE TABLE behind such a comment must still be
    /// detected (token-level, not text `strip_prefix`) and surface a ParseError,
    /// not silently vanish.
    #[test]
    fn commented_unparseable_create_table_surfaces_error() {
        let out = fix_sql_mysql(
            "--\n-- Table structure for table `zf`\n--\nCREATE TABLE zf (c int zerofill);",
        );
        assert!(
            out.diagnostics
                .iter()
                .any(|d| matches!(d.rule, crate::LintRule::ParseError)),
            "comment-prefixed unparseable table must not vanish:\n{}\n{:?}",
            out.sql,
            out.diagnostics
        );
    }

    /// Bug 6: an INSERT (row data) is dropped with an explicit unfixable
    /// diagnostic — never silently, never with corrupted/backticked output. The
    /// surrounding DDL still translates.
    #[test]
    fn insert_dropped_with_diagnostic() {
        let out = fix_sql_mysql(
            "CREATE TABLE `t` (`id` int, `name` varchar(50));\n\
             INSERT INTO `t` VALUES (1,'alice'),(2,'bob''s');",
        );
        assert!(
            out.diagnostics
                .iter()
                .any(|d| matches!(d.rule, crate::LintRule::MysqlDataStatementDropped)),
            "INSERT must produce a data-dropped diagnostic:\n{:?}",
            out.diagnostics
        );
        assert!(
            !out.sql.to_uppercase().contains("INSERT"),
            "no INSERT text in the DDL output:\n{}",
            out.sql
        );
        assert!(
            out.sql.to_uppercase().contains("CREATE TABLE"),
            "the CREATE TABLE still translates:\n{}",
            out.sql
        );
    }

    /// Bug 7: a hex-string default on a numeric column becomes its decimal value.
    /// Re-emitted verbatim, `HexStringLiteral` prints as `X'02'` (a bit-string),
    /// type-incompatible with an integer column.
    #[test]
    fn hex_default_on_numeric_becomes_decimal() {
        let out = fix_sql_mysql("CREATE TABLE `t` (`f` bigint DEFAULT 0x02);");
        assert_clean_dsql(&out);
        assert!(
            out.sql.to_uppercase().contains("DEFAULT 2"),
            "0x02 on a numeric column must become decimal 2:\n{}",
            out.sql
        );
        assert!(
            !out.sql.to_uppercase().contains("X'02'"),
            "no bit-string literal survives:\n{}",
            out.sql
        );
    }

    /// Bug 8: a double-quoted default is a string in MySQL but a quoted
    /// identifier in Postgres; recast to a single-quoted string literal.
    #[test]
    fn double_quoted_default_becomes_string_literal() {
        let out = fix_sql_mysql("CREATE TABLE `t` (`s` varchar(10) DEFAULT \"hi\");");
        assert_clean_dsql(&out);
        assert!(
            out.sql.contains("DEFAULT 'hi'"),
            "double-quoted default must become a single-quoted literal:\n{}",
            out.sql
        );
    }

    /// Bug 9: an absurd `bit(N)` width must not overflow/panic — the width is
    /// clamped to MySQL's max (64 bits = 8 bytes) before padding a recast
    /// default.
    #[test]
    fn oversized_bit_width_does_not_panic() {
        let out = fix_sql_mysql(
            "CREATE TABLE `t` (`b` bit(18446744073709551615) DEFAULT b'1');",
        );
        assert_clean_dsql(&out);
        assert!(
            out.sql.contains(r"'\x0000000000000001'"),
            "bit width clamped to 8 bytes:\n{}",
            out.sql
        );
    }

    /// Bug 10: a backtick buried inside a function call in a CHECK expression is
    /// stripped by the visitor walk (the old hand-rolled match missed
    /// `Expr::Function`).
    #[test]
    fn backtick_in_check_function_stripped() {
        let out = fix_sql_mysql(
            "CREATE TABLE `t` (`j` text, CONSTRAINT `c` CHECK (json_valid(`j`)));",
        );
        assert_clean_dsql(&out);
        assert!(
            out.sql.to_lowercase().contains("json_valid(j)"),
            "backtick inside the function arg must be stripped:\n{}",
            out.sql
        );
    }

    /// Bug 12: mysqldump's `AUTO_INCREMENT=N` table option seeds the identity
    /// with `START WITH N` so new inserts continue past reloaded rows, and the
    /// warning reflects the seed rather than telling the user to reset manually.
    #[test]
    fn auto_increment_table_seed_becomes_start_with() {
        let out = fix_sql_mysql(
            "CREATE TABLE `t` (`id` int NOT NULL AUTO_INCREMENT, PRIMARY KEY(`id`)) \
             ENGINE=InnoDB AUTO_INCREMENT=1001;",
        );
        assert_clean_dsql(&out);
        assert!(
            out.sql.to_uppercase().contains("START WITH 1001"),
            "the AUTO_INCREMENT seed must become START WITH:\n{}",
            out.sql
        );
        assert!(
            has_warning(&out, "seeded from the dump"),
            "the seed warning must mention it was seeded:\n{:?}",
            out.diagnostics
        );
    }

    /// Bug 12 (no seed): without an `AUTO_INCREMENT=N` option the identity has no
    /// START WITH and the warning tells the user to reset it before new inserts.
    #[test]
    fn auto_increment_without_seed_warns_to_reset() {
        let out = fix_sql_mysql(
            "CREATE TABLE `t` (`id` int NOT NULL AUTO_INCREMENT, PRIMARY KEY(`id`));",
        );
        assert_clean_dsql(&out);
        assert!(
            !out.sql.to_uppercase().contains("START WITH"),
            "no seed means no START WITH:\n{}",
            out.sql
        );
        assert!(
            has_warning(&out, "no AUTO_INCREMENT seed"),
            "the unseeded warning must tell the user to reset:\n{:?}",
            out.diagnostics
        );
    }

    /// Bug 13: diagnostics are sorted by source line regardless of which
    /// producer (translation warning vs gate warning) emitted them.
    #[test]
    fn diagnostics_sorted_by_source_line() {
        let out = fix_sql_mysql(
            "CREATE TABLE `a` (`x` int unsigned);\n\
             CREATE TABLE `b` (`k` enum('a','b'), KEY `i` (`k`));",
        );
        let lines: Vec<usize> = out.diagnostics.iter().map(|d| d.line).collect();
        let mut sorted = lines.clone();
        sorted.sort();
        assert_eq!(lines, sorted, "diagnostics must be line-sorted: {lines:?}");
    }

    /// Bug 13: MySQL-translation warnings carry a non-empty statement preview so
    /// JSON consumers don't get `""`.
    #[test]
    fn translation_warnings_carry_statement_preview() {
        let out = fix_sql_mysql("CREATE TABLE `t` (`k` enum('a','b'));");
        let enum_diag = out
            .diagnostics
            .iter()
            .find(|d| matches!(d.rule, crate::LintRule::MysqlEnumToVarchar))
            .expect("enum warning present");
        assert!(
            !enum_diag.statement.is_empty(),
            "translation warning must carry a statement preview"
        );
    }
}



