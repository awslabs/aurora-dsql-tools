//! Core linting engine: parse SQL -> walk AST -> apply rules -> collect diagnostics.

use sqlparser::{
    ast::Statement,
    dialect::{Dialect, PostgreSqlDialect},
    parser::Parser,
    tokenizer::{Token, Tokenizer},
};

use crate::rules;

/// Indicates whether a rule was able to automatically fix the issue it detected.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(
    feature = "serde",
    derive(serde::Serialize),
    serde(tag = "status", content = "detail", rename_all = "snake_case")
)]
pub enum FixResult {
    Fixed(String),
    FixedWithWarning(String),
    Unfixable,
}

/// Identifies which lint rule produced a diagnostic.
///
/// When serialized (via the `serde` feature), each variant becomes its
/// `snake_case` form — e.g. `SerialType` → `"serial_type"`. These strings
/// are the on-wire identifier; variant renames change the JSON output.
///
/// **Not stable for external pattern matching.** The set of variants grows
/// as new rules are added, and existing variants may be renamed or split.
/// External consumers should treat `LintRule` as opaque (or match on the
/// serde string form) rather than relying on exhaustive matches.
#[doc(hidden)]
#[allow(deprecated)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, strum::EnumIter)]
#[cfg_attr(
    feature = "serde",
    derive(serde::Serialize),
    serde(rename_all = "snake_case")
)]
pub enum LintRule {
    SerialType,
    ArrayType,
    // Retained for source compatibility; no diagnostics emit this legacy rule.
    ForeignKey,
    ForeignKeyMatchPartial,
    ForeignKeyEnforced,
    ForeignKeyNotValid,
    TempTable,
    PartitionBy,
    Inherits,
    CreateTableAs,
    Tablespace,
    IdentityType,
    IdentityCache,
    IdentityCacheMissing,
    IndexAsync,
    IndexConcurrently,
    IndexUsing,
    IndexSortDirection,
    IndexPartial,
    Truncate,
    SequenceType,
    SequenceCache,
    SequenceCacheMissing,
    AddColumnConstraint,
    Collation,
    TransactionIsolation,
    SetTransaction,
    // ALTER TABLE operations — one variant per rejected operation arm.
    AtUnsupportedAlterColumnSetType,
    AtUnsupportedAlterColumnSetNotNull,
    AtUnsupportedAlterColumnAddGenerated,
    AtUnsupportedAddCheck,
    AtUnsupportedAddPrimaryKey,
    AtUnsupportedAddUnique,
    // Retained for source compatibility; no diagnostics emit this legacy rule.
    AtUnsupportedDropConstraint,
    AtUnsupportedPrimaryKeyUsingIndex,
    AtUnsupportedUniqueUsingIndex,
    AtUnsupportedRowLevelSecurity,
    AtUnsupportedReplicaIdentity,
    ValidateConstraintAsync,
    AtUnsupportedRewriteRule,
    // Top-level statement rejections — one variant per arm.
    UnsupportedTempView,
    UnsupportedMaterializedView,
    UnsupportedCreateTrigger,
    UnsupportedCreateExtension,
    UnsupportedCreateFunctionNonSql,
    UnsupportedCreateProcedure,
    UnsupportedCreateDatabase,
    UnsupportedCreatePolicy,
    UnsupportedSavepoint,
    UnsupportedReleaseSavepoint,
    UnsupportedRollbackToSavepoint,
    UnsupportedDeclareCursor,
    UnsupportedCreateType,
    UnsupportedCreateServer,
    UnsupportedVacuum,
    UnsupportedAlterIndex,
    UnsupportedCopyFromFile,
    UnsupportedLockTable,
    UnsupportedAlterAggregate,
    UnsupportedAlterFunctionProperty,
    UnsupportedAlterPolicy,
    UnsupportedAlterType,
    UnsupportedAlterRoleProperty,
    UnsupportedAlterRoleSet,
    UnsupportedAlterUser,
    UnsupportedDropMaterializedView,
    UnsupportedDropType,
    UnsupportedDropTrigger,
    UnsupportedDropPolicy,
    UnsupportedListen,
    UnsupportedUnlisten,
    UnsupportedNotify,
    UnsupportedLoad,
    UnsupportedPrepare,
    UnsupportedDeallocate,
    UnsupportedDiscard,
    UnsupportedPartitionOf,
    UnsupportedOnCommit,
    UnsupportedCreateTableWithStorageParameters,
    MultiDdlTransaction,
    MixedDdlDmlTransaction,
    SerialSequenceIdiom,
    AlterAddUniqueCollapse,
    AlterAddPrimaryKeyCollapse,
    // DSQL-native pg_dump idioms (a dump taken from a DSQL cluster emits DDL
    // it cannot itself re-ingest; these collapse/strip it back to a loadable
    // form). See `rules::identity_idiom`.
    IdentityAddGeneratedCollapse,
    AlterColumnSetCompressionStrip,
    // MySQL → DSQL translation warnings (emitted only by `fix_sql_mysql`).
    // Each marks a lossy transform whose output is valid DSQL but not
    // semantically identical to the MySQL source — surfaced as
    // `FixedWithWarning` so `Fixed` stays reserved for faithful rewrites.
    MysqlUnsignedWidened,
    MysqlEnumToVarchar,
    MysqlSetToText,
    MysqlAutoIncrementToIdentity,
    MysqlOnUpdateDropped,
    MysqlInvalidDefaultDropped,
    MysqlIndexPrefixDropped,
    MysqlIndexRenamed,
    MysqlDataStatementDropped,
    ParseError,
}

/// A single compatibility issue found in the input SQL.
///
/// Returned by [`lint_sql`] and consumed by both the CLI (for human-readable output)
/// and the library crate (for programmatic integration, e.g. in MCP servers).
#[derive(Debug, Clone)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct Diagnostic {
    pub rule: LintRule,
    pub line: usize,
    /// Raw SQL of the offending statement. Excluded from `Serialize` via
    /// `#[serde(skip)]` because it can be long and multi-line; callers that
    /// want it in JSON should wrap `Diagnostic` in their own type (the CLI
    /// uses a `statement_preview` field for this).
    #[cfg_attr(feature = "serde", serde(skip))]
    pub statement: String,
    pub message: String,
    pub suggestion: String,
    pub fix_result: FixResult,
}

/// Precompute byte offset of each line start for (line, col) -> byte conversion.
fn line_byte_offsets(input: &str) -> Vec<usize> {
    let mut offsets = vec![0];
    for (i, b) in input.bytes().enumerate() {
        if b == b'\n' {
            offsets.push(i + 1);
        }
    }
    offsets
}

/// Convert a 1-based (line, column) to a byte offset. Column counts Unicode scalar values.
fn loc_to_byte(input: &str, offsets: &[usize], line: u64, col: u64) -> usize {
    let line_idx = (line as usize).saturating_sub(1);
    let line_start = offsets.get(line_idx).copied().unwrap_or(0);
    let col_chars = (col as usize).saturating_sub(1);
    input[line_start..]
        .char_indices()
        .nth(col_chars)
        .map(|(byte_off, _)| line_start + byte_off)
        .unwrap_or(input.len())
}

/// Split SQL input into `(line_number, statement_text)` pairs on `;`.
///
/// Uses the tokenizer to correctly handle semicolons inside quoted strings or
/// comments. Preserves original text (including newlines) by slicing the input
/// using token span byte offsets rather than reconstructing from token strings.
///
/// `pub(crate)` so the `grammar-diff` binary, via the `crate::grammar`
/// re-export, can reuse the exact same splitter and avoid an
/// apples-to-oranges diff between what the lint engine sees per statement
/// and what the grammar oracle sees per statement.
pub(crate) fn split_statements(input: &str) -> Result<Vec<(usize, String)>, String> {
    split_statements_dialect(input, &PostgreSqlDialect {})
}

/// Dialect-generic statement splitter. `fix_sql_mysql` reuses this with
/// `MySqlDialect` to slice statement text from the source bytes — rebuilding
/// from tokens double-unescapes string literals and corrupts data.
pub(crate) fn split_statements_dialect(
    input: &str,
    dialect: &dyn Dialect,
) -> Result<Vec<(usize, String)>, String> {
    let all_tokens = Tokenizer::new(dialect, input)
        .tokenize_with_location()
        .map_err(|e| e.to_string())?;

    let offsets = line_byte_offsets(input);
    let mut results = Vec::new();
    let mut stmt_first_line: Option<u64> = None;
    let mut stmt_start_byte: Option<usize> = None;
    let mut stmt_end_byte: usize = 0;

    for twl in &all_tokens {
        match &twl.token {
            Token::Whitespace(_) => {}
            Token::SemiColon => {
                if let (Some(start), Some(line)) = (stmt_start_byte, stmt_first_line) {
                    let text = &input[start..stmt_end_byte];
                    if !text.trim().is_empty() {
                        results.push((line as usize, text.to_string()));
                    }
                }
                stmt_start_byte = None;
                stmt_first_line = None;
            }
            _ => {
                let tok_start =
                    loc_to_byte(input, &offsets, twl.span.start.line, twl.span.start.column);
                let tok_end_incl =
                    loc_to_byte(input, &offsets, twl.span.end.line, twl.span.end.column);
                // Span.end is inclusive; advance past the last character.
                let tok_end = input[tok_end_incl..]
                    .chars()
                    .next()
                    .map(|c| tok_end_incl + c.len_utf8())
                    .unwrap_or(input.len());

                if stmt_start_byte.is_none() {
                    stmt_start_byte = Some(tok_start);
                    stmt_first_line = Some(twl.span.start.line);
                }
                stmt_end_byte = tok_end;
            }
        }
    }

    // Flush any remaining statement (tokenize_with_location does not emit EOF)
    if let (Some(start), Some(line)) = (stmt_start_byte, stmt_first_line) {
        let text = &input[start..stmt_end_byte];
        if !text.trim().is_empty() {
            results.push((line as usize, text.to_string()));
        }
    }

    Ok(results)
}

fn is_unquoted_keyword(token: &Token, expected: &str) -> bool {
    matches!(
        token,
        Token::Word(word)
            if word.quote_style.is_none() && word.value.eq_ignore_ascii_case(expected)
    )
}

fn consume_object_name(tokens: &[Token], start: usize) -> Option<usize> {
    if !matches!(tokens.get(start), Some(Token::Word(_))) {
        return None;
    }

    let mut next = start + 1;
    while matches!(tokens.get(next), Some(Token::Period)) {
        if !matches!(tokens.get(next + 1), Some(Token::Word(_))) {
            return None;
        }
        next += 2;
    }
    Some(next)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum FkSetAction {
    Null,
    Default,
}

impl FkSetAction {
    const fn index(self) -> usize {
        match self {
            Self::Null => 0,
            Self::Default => 1,
        }
    }
}

struct FkActionColumnList {
    action: FkSetAction,
    occurrence: usize,
    columns: String,
}

struct FkParserGap {
    parseable: String,
    action_column_lists: Vec<FkActionColumnList>,
}

/// Remove the optional column list from `ON DELETE SET NULL/SET DEFAULT`.
/// The current parser release doesn't model this PostgreSQL syntax yet, but
/// DSQL supports it. Parsing the otherwise-equivalent statement still lets
/// the linter apply all other foreign-key rules.
fn without_fk_action_column_lists(stmt_text: &str) -> Option<FkParserGap> {
    let dialect = PostgreSqlDialect {};
    let Ok(tokens) = Tokenizer::new(&dialect, stmt_text).tokenize() else {
        return None;
    };
    let tokens: Vec<_> = tokens
        .into_iter()
        .filter(|token| !matches!(token, Token::Whitespace(_) | Token::SemiColon))
        .collect();
    let mut output = Vec::with_capacity(tokens.len());
    let mut action_column_lists = Vec::new();
    let mut action_occurrences = [0; 2];
    let mut i = 0;

    while i < tokens.len() {
        let action = if i >= 3
            && is_unquoted_keyword(&tokens[i - 3], "ON")
            && is_unquoted_keyword(&tokens[i - 2], "DELETE")
            && is_unquoted_keyword(&tokens[i - 1], "SET")
        {
            if is_unquoted_keyword(&tokens[i], "NULL") {
                Some(FkSetAction::Null)
            } else if is_unquoted_keyword(&tokens[i], "DEFAULT") {
                Some(FkSetAction::Default)
            } else {
                None
            }
        } else {
            None
        };
        output.push(tokens[i].clone());
        i += 1;

        let Some(action) = action else {
            continue;
        };
        let occurrence = action_occurrences[action.index()];
        action_occurrences[action.index()] += 1;

        if !matches!(tokens.get(i), Some(Token::LParen)) {
            continue;
        }

        let mut next = i + 1;
        let mut expect_name = true;
        let mut columns = Vec::new();
        while next < tokens.len() {
            match tokens.get(next) {
                Some(Token::Word(_)) if expect_name => {
                    columns.push(tokens[next].to_string());
                    expect_name = false;
                    next += 1;
                }
                Some(Token::Comma) if !expect_name => {
                    expect_name = true;
                    next += 1;
                }
                Some(Token::RParen) if !expect_name && !columns.is_empty() => {
                    action_column_lists.push(FkActionColumnList {
                        action,
                        occurrence,
                        columns: format!("({})", columns.join(", ")),
                    });
                    i = next + 1;
                    break;
                }
                _ => break,
            }
        }
    }

    (!action_column_lists.is_empty()).then(|| FkParserGap {
        parseable: output
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(" "),
        action_column_lists,
    })
}

fn restore_fk_action_column_lists(sql: &str, gap: &FkParserGap) -> Result<String, String> {
    let dialect = PostgreSqlDialect {};
    let tokenized = Tokenizer::new(&dialect, sql)
        .tokenize_with_location()
        .map_err(|error| error.to_string())?;
    let tokens: Vec<_> = tokenized
        .iter()
        .filter(|token| !matches!(&token.token, Token::Whitespace(_) | Token::SemiColon))
        .collect();
    let offsets = line_byte_offsets(sql);
    let mut action_occurrences = [0; 2];
    let mut insertions = Vec::with_capacity(gap.action_column_lists.len());

    for start in 0..tokens.len().saturating_sub(3) {
        if !is_unquoted_keyword(&tokens[start].token, "ON")
            || !is_unquoted_keyword(&tokens[start + 1].token, "DELETE")
            || !is_unquoted_keyword(&tokens[start + 2].token, "SET")
        {
            continue;
        }
        let action = if is_unquoted_keyword(&tokens[start + 3].token, "NULL") {
            FkSetAction::Null
        } else if is_unquoted_keyword(&tokens[start + 3].token, "DEFAULT") {
            FkSetAction::Default
        } else {
            continue;
        };
        let occurrence = action_occurrences[action.index()];
        action_occurrences[action.index()] += 1;

        let Some(action_column_list) = gap
            .action_column_lists
            .iter()
            .find(|entry| entry.action == action && entry.occurrence == occurrence)
        else {
            continue;
        };
        let action_token = tokens[start + 3];
        let insert_at = loc_to_byte(
            sql,
            &offsets,
            action_token.span.end.line,
            action_token.span.end.column,
        );
        insertions.push((insert_at, format!(" {}", action_column_list.columns)));
    }

    if insertions.len() != gap.action_column_lists.len() {
        return Err(format!(
            "restored {} of {} foreign-key action column lists",
            insertions.len(),
            gap.action_column_lists.len()
        ));
    }

    let mut restored = sql.to_string();
    insertions.sort_by_key(|(insert_at, _)| *insert_at);
    for (insert_at, text) in insertions.into_iter().rev() {
        restored.insert_str(insert_at, &text);
    }

    Ok(restored)
}

/// `sqlparser-dsql` does not yet parse PostgreSQL's ALTER CONSTRAINT
/// deferrability operation, even though DSQL supports it for foreign keys.
/// Recognize only the supported token shapes so malformed variants still
/// surface as parse errors.
fn is_supported_alter_constraint(stmt_text: &str) -> bool {
    let dialect = PostgreSqlDialect {};
    let Ok(tokens) = Tokenizer::new(&dialect, stmt_text).tokenize() else {
        return false;
    };
    let tokens: Vec<_> = tokens
        .into_iter()
        .filter(|token| !matches!(token, Token::Whitespace(_) | Token::SemiColon))
        .collect();

    if !tokens
        .first()
        .is_some_and(|token| is_unquoted_keyword(token, "ALTER"))
        || !tokens
            .get(1)
            .is_some_and(|token| is_unquoted_keyword(token, "TABLE"))
    {
        return false;
    }

    let mut next = 2;
    if tokens
        .get(next)
        .is_some_and(|token| is_unquoted_keyword(token, "IF"))
        && tokens
            .get(next + 1)
            .is_some_and(|token| is_unquoted_keyword(token, "EXISTS"))
    {
        next += 2;
    }
    if tokens
        .get(next)
        .is_some_and(|token| is_unquoted_keyword(token, "ONLY"))
    {
        next += 1;
    }
    let Some(after_table) = consume_object_name(&tokens, next) else {
        return false;
    };
    next = after_table;
    if matches!(tokens.get(next), Some(Token::Mul)) {
        next += 1;
    }

    if !tokens
        .get(next)
        .is_some_and(|token| is_unquoted_keyword(token, "ALTER"))
        || !tokens
            .get(next + 1)
            .is_some_and(|token| is_unquoted_keyword(token, "CONSTRAINT"))
        || !matches!(tokens.get(next + 2), Some(Token::Word(_)))
    {
        return false;
    }
    next += 3;

    let remaining = &tokens[next..];
    (remaining.len() == 1 && is_unquoted_keyword(&remaining[0], "DEFERRABLE"))
        || (remaining.len() == 2
            && is_unquoted_keyword(&remaining[0], "NOT")
            && is_unquoted_keyword(&remaining[1], "DEFERRABLE"))
        || (remaining.len() == 2
            && is_unquoted_keyword(&remaining[0], "INITIALLY")
            && (is_unquoted_keyword(&remaining[1], "DEFERRED")
                || is_unquoted_keyword(&remaining[1], "IMMEDIATE")))
        || (remaining.len() == 3
            && is_unquoted_keyword(&remaining[0], "DEFERRABLE")
            && is_unquoted_keyword(&remaining[1], "INITIALLY")
            && (is_unquoted_keyword(&remaining[2], "DEFERRED")
                || is_unquoted_keyword(&remaining[2], "IMMEDIATE")))
        || (remaining.len() == 4
            && is_unquoted_keyword(&remaining[0], "NOT")
            && is_unquoted_keyword(&remaining[1], "DEFERRABLE")
            && is_unquoted_keyword(&remaining[2], "INITIALLY")
            && (is_unquoted_keyword(&remaining[3], "DEFERRED")
                || is_unquoted_keyword(&remaining[3], "IMMEDIATE")))
}

fn is_supported_set_constraints(stmt_text: &str) -> bool {
    let dialect = PostgreSqlDialect {};
    let Ok(tokens) = Tokenizer::new(&dialect, stmt_text).tokenize() else {
        return false;
    };
    let tokens: Vec<_> = tokens
        .into_iter()
        .filter(|token| !matches!(token, Token::Whitespace(_) | Token::SemiColon))
        .collect();

    if tokens.len() < 4
        || !is_unquoted_keyword(&tokens[0], "SET")
        || !is_unquoted_keyword(&tokens[1], "CONSTRAINTS")
        || !(is_unquoted_keyword(tokens.last().unwrap(), "DEFERRED")
            || is_unquoted_keyword(tokens.last().unwrap(), "IMMEDIATE"))
    {
        return false;
    }

    let targets = &tokens[2..tokens.len() - 1];
    if targets.len() == 1 && is_unquoted_keyword(&targets[0], "ALL") {
        return true;
    }
    if targets
        .iter()
        .any(|token| is_unquoted_keyword(token, "ALL"))
    {
        return false;
    }

    let mut next = 0;
    while next < targets.len() {
        let Some(after_name) = consume_object_name(targets, next) else {
            return false;
        };
        next = after_name;
        if next == targets.len() {
            return true;
        }
        if !matches!(targets.get(next), Some(Token::Comma)) {
            return false;
        }
        next += 1;
    }
    false
}

fn is_supported_parser_gap_statement(stmt_text: &str) -> bool {
    is_supported_alter_constraint(stmt_text) || is_supported_set_constraints(stmt_text)
}

fn is_ddl(stmt: &Statement) -> bool {
    matches!(
        stmt,
        Statement::CreateTable(_)
            | Statement::CreateIndex(_)
            | Statement::CreateView(_)
            | Statement::CreateSequence { .. }
            | Statement::CreateType { .. }
            | Statement::CreateFunction(_)
            | Statement::CreateProcedure { .. }
            | Statement::CreateTrigger(_)
            | Statement::CreateExtension(_)
            | Statement::CreateSchema { .. }
            | Statement::CreateDatabase { .. }
            | Statement::CreatePolicy(_)
            | Statement::CreateServer(_)
            | Statement::AlterTable(_)
            | Statement::AlterIndex { .. }
            | Statement::AlterFunction(_)
            | Statement::AlterPolicy(_)
            | Statement::AlterType(_)
            | Statement::AlterRole { .. }
            | Statement::AlterUser(_)
            | Statement::Drop { .. }
            | Statement::DropTrigger(_)
            | Statement::DropPolicy(_)
            | Statement::Truncate(_)
    )
}

fn is_dml(stmt: &Statement) -> bool {
    matches!(
        stmt,
        Statement::Insert(_)
            | Statement::Update { .. }
            | Statement::Delete(_)
            | Statement::Merge { .. }
    )
}

fn is_begin(stmt: &Statement) -> bool {
    matches!(stmt, Statement::StartTransaction { .. })
}

fn is_txn_end(stmt: &Statement) -> bool {
    matches!(stmt, Statement::Commit { .. } | Statement::Rollback { .. })
}

fn is_commit(stmt: &Statement) -> bool {
    matches!(stmt, Statement::Commit { .. })
}

fn multi_ddl_txn_diagnostic(
    line: usize,
    ddl_count: usize,
    begin_text: &str,
    fix_result: FixResult,
) -> Diagnostic {
    Diagnostic {
        rule: LintRule::MultiDdlTransaction,
        line,
        statement: begin_text.to_string(),
        message: format!(
            "Transaction contains {ddl_count} DDL statements. DSQL supports only one DDL statement per transaction."
        ),
        suggestion: "Split into separate transactions: wrap each DDL statement in its own BEGIN/COMMIT block. Note: this changes semantics — the original transaction's atomicity guarantee is lost. If a later statement fails, earlier statements remain committed.".to_string(),
        fix_result,
    }
}

fn mixed_ddl_dml_txn_diagnostic(
    line: usize,
    ddl_count: usize,
    dml_count: usize,
    begin_text: &str,
    fix_result: FixResult,
) -> Diagnostic {
    Diagnostic {
        rule: LintRule::MixedDdlDmlTransaction,
        line,
        statement: begin_text.to_string(),
        message: format!(
            "Transaction mixes DDL and DML ({ddl_count} DDL, {dml_count} DML). DSQL does not allow DDL and DML in the same transaction."
        ),
        suggestion: "Split into separate transactions so each BEGIN/COMMIT block contains either DDL or DML, not both. Note: this changes semantics — the original transaction's atomicity guarantee is lost. If a later statement fails, earlier statements remain committed.".to_string(),
        fix_result,
    }
}

/// Cross-statement pass: detect transaction blocks (BEGIN … COMMIT) that
/// violate DSQL's single-transaction constraints — either >1 DDL statement,
/// or any mix of DDL and DML.
fn check_ddl_transactions(stmts: &[(usize, String)], diagnostics: &mut Vec<Diagnostic>) {
    let dialect = PostgreSqlDialect {};
    let mut in_txn = false;
    let mut txn_begin_line: usize = 0;
    let mut txn_begin_text = String::new();
    let mut ddl_count: usize = 0;
    let mut dml_count: usize = 0;

    for (line_num, stmt_text) in stmts {
        let parsed = match Parser::parse_sql(&dialect, stmt_text.trim()) {
            Ok(p) => p,
            Err(e) => {
                if is_supported_alter_constraint(stmt_text) {
                    if in_txn {
                        ddl_count += 1;
                    }
                    continue;
                }
                if is_supported_set_constraints(stmt_text) {
                    continue;
                }
                if in_txn {
                    diagnostics.push(Diagnostic {
                        rule: LintRule::ParseError,
                        line: *line_num,
                        statement: stmt_text.to_string(),
                        message: format!(
                            "Cannot parse statement inside transaction block: {e}. DDL transaction analysis may be incomplete."
                        ),
                        suggestion: "Fix the SQL syntax or manually verify this transaction has at most one DDL statement.".to_string(),
                        fix_result: FixResult::Unfixable,
                    });
                }
                continue;
            }
        };

        for stmt in &parsed {
            if is_begin(stmt) && !in_txn {
                in_txn = true;
                txn_begin_line = *line_num;
                txn_begin_text = stmt_text.to_string();
                ddl_count = 0;
                dml_count = 0;
            } else if is_txn_end(stmt) {
                if in_txn && is_commit(stmt) {
                    if ddl_count > 1 {
                        diagnostics.push(multi_ddl_txn_diagnostic(
                            txn_begin_line,
                            ddl_count,
                            &txn_begin_text,
                            FixResult::Unfixable,
                        ));
                    }
                    if ddl_count >= 1 && dml_count >= 1 {
                        diagnostics.push(mixed_ddl_dml_txn_diagnostic(
                            txn_begin_line,
                            ddl_count,
                            dml_count,
                            &txn_begin_text,
                            FixResult::Unfixable,
                        ));
                    }
                }
                in_txn = false;
            } else if in_txn {
                if is_ddl(stmt) {
                    ddl_count += 1;
                } else if is_dml(stmt) {
                    dml_count += 1;
                }
            }
        }
    }
}

/// Fix pass: split transaction blocks that DSQL would reject — either >1
/// DDL statement, or a mix of DDL and DML. Each DDL ends up in its own
/// BEGIN/COMMIT wrapper; runs of non-DDL statements are bundled into their
/// own block.
fn fix_ddl_transactions(parts: &mut Vec<(usize, String)>, diagnostics: &mut Vec<Diagnostic>) {
    let dialect = PostgreSqlDialect {};

    let mut i = 0;
    'outer: while i < parts.len() {
        let parsed = match Parser::parse_sql(&dialect, parts[i].1.trim()) {
            Ok(p) => p,
            Err(_) => {
                i += 1;
                continue;
            }
        };

        if !parsed.iter().any(is_begin) {
            i += 1;
            continue;
        }

        let begin_idx = i;
        let begin_line = parts[begin_idx].0;
        let mut ddl_indices = Vec::new();
        let mut dml_count = 0;
        let mut commit_idx = None;

        let mut nested_begin_indices = Vec::new();
        'txn: for (j, (line, text)) in parts.iter().enumerate().skip(begin_idx + 1) {
            if is_supported_alter_constraint(text) {
                ddl_indices.push(j);
                continue;
            }
            if is_supported_set_constraints(text) {
                continue;
            }
            let p = match Parser::parse_sql(&dialect, text.trim()) {
                Ok(p) => p,
                Err(e) => {
                    diagnostics.push(Diagnostic {
                        rule: LintRule::ParseError,
                        line: *line,
                        statement: text.to_string(),
                        message: format!(
                            "Cannot parse statement inside transaction: {e}. Skipping auto-fix for this transaction block."
                        ),
                        suggestion: "Fix the syntax error, then re-run with --fix.".to_string(),
                        fix_result: FixResult::Unfixable,
                    });
                    i += 1;
                    continue 'outer;
                }
            };
            if p.iter().any(is_txn_end) {
                if p.iter().any(is_commit) {
                    commit_idx = Some(j);
                }
                break 'txn;
            }
            if p.iter().any(is_begin) {
                nested_begin_indices.push(j);
            } else if p.iter().any(is_ddl) {
                ddl_indices.push(j);
            } else if p.iter().any(is_dml) {
                dml_count += 1;
            }
        }

        let commit_idx = match commit_idx {
            Some(idx) => idx,
            None => {
                i += 1;
                continue;
            }
        };

        let ddl_count = ddl_indices.len();
        let needs_split = ddl_count > 1 || (ddl_count >= 1 && dml_count >= 1);
        if !needs_split {
            i = commit_idx + 1;
            continue;
        }

        let begin_text = parts[begin_idx].1.clone();

        let mut replacement: Vec<(usize, String)> = Vec::new();
        let mut pending_non_ddl: Vec<(usize, String)> = Vec::new();

        for (j, part) in parts
            .iter()
            .enumerate()
            .take(commit_idx)
            .skip(begin_idx + 1)
        {
            if nested_begin_indices.contains(&j) {
                continue;
            } else if ddl_indices.contains(&j) {
                if !pending_non_ddl.is_empty() {
                    replacement.push((begin_line, begin_text.clone()));
                    replacement.append(&mut pending_non_ddl);
                    replacement.push((begin_line, "COMMIT".to_string()));
                }
                replacement.push((begin_line, begin_text.clone()));
                replacement.push(part.clone());
                replacement.push((begin_line, "COMMIT".to_string()));
            } else {
                pending_non_ddl.push(part.clone());
            }
        }
        if !pending_non_ddl.is_empty() {
            replacement.push((begin_line, begin_text.clone()));
            replacement.append(&mut pending_non_ddl);
            replacement.push((begin_line, "COMMIT".to_string()));
        }

        let replacement_len = replacement.len();
        let range_len = commit_idx - begin_idx + 1;
        parts.splice(begin_idx..begin_idx + range_len, replacement);

        if ddl_count > 1 {
            diagnostics.push(multi_ddl_txn_diagnostic(
                begin_line,
                ddl_count,
                &begin_text,
                FixResult::FixedWithWarning(
                    "Split multi-DDL transaction into individual BEGIN/COMMIT blocks; atomicity guarantee LOST — if a later statement fails after fix, earlier statements remain committed. Review carefully before applying.".to_string(),
                ),
            ));
        }
        if ddl_count >= 1 && dml_count >= 1 {
            diagnostics.push(mixed_ddl_dml_txn_diagnostic(
                begin_line,
                ddl_count,
                dml_count,
                &begin_text,
                FixResult::FixedWithWarning(
                    "Split mixed DDL+DML transaction; atomicity guarantee LOST — if a later statement fails after fix, earlier statements remain committed. Review carefully before applying.".to_string(),
                ),
            ));
        }

        i = begin_idx + replacement_len;
    }
}

/// Per-statement rules in `errors.rs` take `&mut Statement` and may
/// mutate the AST — one code path for both lint and fix, so the two
/// modes can't drift. Multi-statement idiom rules (`serial_idiom`,
/// `constraint_collapse`) expose paired `check_*` / `fix_*` entry
/// points instead, since their fix mode rewrites and removes parts
/// rather than mutating a single AST.
pub fn lint_sql(sql: &str) -> Vec<Diagnostic> {
    let dialect = PostgreSqlDialect {};
    let mut diagnostics = Vec::new();

    let stmts = match split_statements(sql) {
        Ok(s) => s,
        Err(e) => {
            diagnostics.push(Diagnostic {
                rule: LintRule::ParseError,
                line: 1,
                statement: String::new(),
                message: format!("Failed to tokenize SQL: {e}"),
                suggestion: "Fix the SQL syntax and try again.".to_string(),
                fix_result: FixResult::Unfixable,
            });
            return diagnostics;
        }
    };

    // Pre-passes: surface multi-statement idioms (SERIAL expansion,
    // standalone PK/UNIQUE ALTERs) as a single high-level diagnostic.
    // The per-statement loop still runs (this is lint, not fix), so
    // the lower-level Unfixable rules also fire alongside.
    rules::serial_idiom::check_serial_idioms(&stmts, &mut diagnostics);
    rules::constraint_collapse::check_alter_add_unique(&stmts, &mut diagnostics);
    rules::constraint_collapse::check_alter_add_primary_key(&stmts, &mut diagnostics);
    rules::identity_idiom::check_identity_adds(&stmts, &mut diagnostics);
    rules::identity_idiom::check_set_compression(&stmts, &mut diagnostics);

    for (line_num, stmt_text) in &stmts {
        if stmt_text.trim().is_empty() {
            continue;
        }

        let mut parsed = match Parser::parse_sql(&dialect, stmt_text).or_else(|original_error| {
            let Some(parser_gap) = without_fk_action_column_lists(stmt_text) else {
                return Err(original_error);
            };
            Parser::parse_sql(&dialect, &parser_gap.parseable)
        }) {
            Ok(p) => p,
            Err(e) => {
                if is_supported_parser_gap_statement(stmt_text) {
                    continue;
                }
                diagnostics.push(Diagnostic {
                    rule: LintRule::ParseError,
                    line: *line_num,
                    statement: stmt_text.to_string(),
                    message: format!("Failed to parse SQL: {e}"),
                    suggestion: "Fix the SQL syntax and try again.".to_string(),
                    fix_result: FixResult::Unfixable,
                });
                continue;
            }
        };

        for stmt in &mut parsed {
            let mut stmt_diags = Vec::new();
            rules::check_statement(stmt, stmt_text, &mut stmt_diags);

            // Rules report line numbers relative to their statement;
            // translate to absolute line numbers in the original input.
            for d in &mut stmt_diags {
                d.line = line_num + d.line - 1;
                d.statement = stmt_text.to_string();
            }
            diagnostics.extend(stmt_diags);
        }
    }

    check_ddl_transactions(&stmts, &mut diagnostics);

    diagnostics
}

pub struct FixOutput {
    pub sql: String,
    pub diagnostics: Vec<Diagnostic>,
}

pub fn fix_sql(sql: &str) -> FixOutput {
    let stmts = match split_statements(sql) {
        Ok(s) => s,
        Err(e) => {
            return FixOutput {
                sql: sql.to_string(),
                diagnostics: vec![Diagnostic {
                    rule: LintRule::ParseError,
                    line: 1,
                    statement: String::new(),
                    message: format!("Failed to tokenize SQL: {e}"),
                    suggestion: "Fix the SQL syntax and try again.".to_string(),
                    fix_result: FixResult::Unfixable,
                }],
            };
        }
    };
    fix_statements(stmts)
}

/// The DSQL-compatibility gate, entered from already-split `(line, text)`
/// statements. `fix_sql_mysql` calls this directly with source line numbers, so
/// gate diagnostics need no remap. Each statement parses independently, so one
/// unparseable statement can't disable the gate for the rest.
pub(crate) fn fix_statements(mut stmts: Vec<(usize, String)>) -> FixOutput {
    let dialect = PostgreSqlDialect {};
    let mut all_diagnostics = Vec::new();
    let mut fixed_parts: Vec<(usize, String)> = Vec::new();

    // Pre-passes: collapse multi-statement idioms BEFORE the per-statement
    // loop, so the loop never emits Unfixable diagnostics on statements
    // we just folded away (or ParseError on the unparseable
    // `ALTER SEQUENCE ... OWNED BY` line that the SERIAL idiom drops).
    rules::serial_idiom::fix_serial_idioms(&mut stmts, &mut all_diagnostics);
    rules::constraint_collapse::fix_alter_add_unique(&mut stmts, &mut all_diagnostics);
    rules::constraint_collapse::fix_alter_add_primary_key(&mut stmts, &mut all_diagnostics);
    rules::identity_idiom::fix_identity_adds(&mut stmts, &mut all_diagnostics);
    rules::identity_idiom::fix_set_compression(&mut stmts, &mut all_diagnostics);

    for (line_num, stmt_text) in &stmts {
        if stmt_text.trim().is_empty() {
            fixed_parts.push((*line_num, stmt_text.to_string()));
            continue;
        }

        let parser_gap = without_fk_action_column_lists(stmt_text);
        let mut parsed = match Parser::parse_sql(&dialect, stmt_text).or_else(|original_error| {
            let Some(parser_gap) = parser_gap.as_ref() else {
                return Err(original_error);
            };
            Parser::parse_sql(&dialect, &parser_gap.parseable)
        }) {
            Ok(p) => p,
            Err(e) => {
                fixed_parts.push((*line_num, stmt_text.trim_end_matches(';').to_string()));
                if is_supported_parser_gap_statement(stmt_text) {
                    continue;
                }
                all_diagnostics.push(Diagnostic {
                    rule: LintRule::ParseError,
                    line: *line_num,
                    statement: stmt_text.to_string(),
                    message: format!("Failed to parse SQL: {e}"),
                    suggestion: "Fix the SQL syntax and try again.".to_string(),
                    fix_result: FixResult::Unfixable,
                });
                continue;
            }
        };

        let mut stmt_diags = Vec::new();

        for stmt in &mut parsed {
            rules::check_statement(stmt, stmt_text, &mut stmt_diags);
        }

        let modified = stmt_diags.iter().any(|d| {
            matches!(
                d.fix_result,
                FixResult::Fixed(_) | FixResult::FixedWithWarning(_)
            )
        });

        let is_empty_alter = matches!(
            parsed.first(),
            Some(Statement::AlterTable(at)) if at.operations.is_empty()
        );

        if parsed.is_empty() || is_empty_alter {
            // Statement was removed entirely (e.g. ALTER TABLE with all FK ops stripped)
        } else if modified {
            let mut fixed = parsed
                .iter()
                .map(|s| format!("{:#}", s))
                .collect::<Vec<_>>()
                .join(";\n");
            if let Some(parser_gap) = &parser_gap {
                match restore_fk_action_column_lists(&fixed, parser_gap) {
                    Ok(restored) => fixed = restored,
                    Err(error) => {
                        fixed = stmt_text.trim_end_matches(';').to_string();
                        for diagnostic in &mut stmt_diags {
                            if matches!(
                                diagnostic.fix_result,
                                FixResult::Fixed(_) | FixResult::FixedWithWarning(_)
                            ) {
                                diagnostic.fix_result = FixResult::Unfixable;
                            }
                        }
                        stmt_diags.push(Diagnostic {
                            rule: LintRule::ParseError,
                            line: 1,
                            statement: String::new(),
                            message: format!(
                                "Failed to restore foreign-key action column lists: {error}"
                            ),
                            suggestion:
                                "Preserve each ON DELETE SET NULL/SET DEFAULT column list manually."
                                    .to_string(),
                            fix_result: FixResult::Unfixable,
                        });
                    }
                }
            }
            fixed_parts.push((*line_num, fixed));
        } else {
            fixed_parts.push((*line_num, stmt_text.trim_end_matches(';').to_string()));
        }

        for d in &mut stmt_diags {
            d.line = line_num + d.line - 1;
            d.statement = stmt_text.to_string();
        }
        all_diagnostics.extend(stmt_diags);
    }

    fix_ddl_transactions(&mut fixed_parts, &mut all_diagnostics);

    let mut sql = fixed_parts
        .iter()
        .map(|(_, s)| s.as_str())
        .collect::<Vec<_>>()
        .join(";\n\n");
    if !sql.is_empty() {
        sql.push_str(";\n");
    }
    FixOutput {
        sql,
        diagnostics: all_diagnostics,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_valid_create_table_no_errors() {
        let sql = "CREATE TABLE orders (id UUID PRIMARY KEY, amount DECIMAL(10,2));";
        let diags = lint_sql(sql);
        assert!(diags.is_empty(), "Expected no errors, got: {:?}", diags);
    }

    #[test]
    fn test_parse_error_returns_diagnostic() {
        let sql = "NOT VALID SQL AT ALL ???";
        let diags = lint_sql(sql);
        assert!(!diags.is_empty());
        assert!(diags[0].message.contains("Failed to parse SQL"));
    }

    #[test]
    fn test_supported_alter_constraint_bypasses_parser_gap() {
        let supported = [
            "ALTER TABLE child ALTER CONSTRAINT child_parent_fkey DEFERRABLE;",
            "ALTER TABLE child ALTER CONSTRAINT child_parent_fkey NOT DEFERRABLE;",
            "ALTER TABLE child ALTER CONSTRAINT child_parent_fkey DEFERRABLE INITIALLY DEFERRED;",
            "ALTER TABLE ONLY public.child ALTER CONSTRAINT child_parent_fkey DEFERRABLE INITIALLY IMMEDIATE;",
            "ALTER TABLE IF EXISTS ONLY public.child * ALTER CONSTRAINT child_parent_fkey INITIALLY DEFERRED;",
            "ALTER TABLE child ALTER CONSTRAINT child_parent_fkey NOT DEFERRABLE INITIALLY IMMEDIATE;",
        ];

        for sql in supported {
            assert!(lint_sql(sql).is_empty(), "expected supported SQL: {sql}");
            let fixed = fix_sql(sql);
            assert!(
                fixed.diagnostics.is_empty(),
                "expected no fix diagnostics for: {sql}"
            );
            assert_eq!(fixed.sql, format!("{sql}\n"));
        }
    }

    #[test]
    fn test_invalid_alter_constraint_still_reports_parse_error() {
        let sql = "ALTER TABLE child ALTER CONSTRAINT child_parent_fkey SOMETIMES DEFERRED;";
        let diags = lint_sql(sql);
        assert!(
            diags
                .iter()
                .any(|diag| matches!(diag.rule, LintRule::ParseError)),
            "invalid deferrability combination must not bypass parsing: {diags:?}"
        );
    }

    #[test]
    fn test_fk_set_action_column_lists_bypass_parser_gap() {
        let supported = [
            "CREATE TABLE parent (a INT, b INT, PRIMARY KEY (a, b)); CREATE TABLE child (a INT, b INT, FOREIGN KEY (a, b) REFERENCES parent (a, b) ON DELETE SET NULL (b));",
            "CREATE TABLE parent (a INT, b INT, PRIMARY KEY (a, b)); CREATE TABLE child (a INT DEFAULT 1, b INT DEFAULT 2, FOREIGN KEY (a, b) REFERENCES parent (a, b) ON DELETE SET DEFAULT (b));",
        ];

        for sql in supported {
            assert!(
                !lint_sql(sql)
                    .iter()
                    .any(|diag| matches!(diag.rule, LintRule::ParseError)),
                "expected supported SQL: {sql}"
            );
        }
    }

    #[test]
    fn test_fk_set_action_column_list_preserved_when_adding_not_valid() {
        let sql = "ALTER TABLE child ADD CONSTRAINT child_parent_fk FOREIGN KEY (a, b) REFERENCES parent (a, b) ON DELETE SET NULL (b);";
        let fixed = fix_sql(sql);
        assert!(fixed.sql.contains("ON DELETE SET NULL (b)"));
        assert!(fixed.sql.contains("NOT VALID"));
        assert!(!fixed
            .diagnostics
            .iter()
            .any(|diag| matches!(diag.rule, LintRule::ParseError)));
    }

    #[test]
    fn test_fk_set_action_column_list_preserved_with_other_alter_operations() {
        let sql = "ALTER TABLE child ADD CONSTRAINT child_parent_fk FOREIGN KEY (a, b) REFERENCES parent (a, b) ON DELETE SET DEFAULT (b), ADD COLUMN note TEXT;";
        let fixed = fix_sql(sql);
        assert!(
            fixed
                .sql
                .contains("ON DELETE SET DEFAULT (b) NOT VALID, ADD COLUMN note TEXT"),
            "unexpected fixed SQL: {}",
            fixed.sql
        );
        assert!(
            lint_sql(&fixed.sql).is_empty(),
            "{:?}",
            lint_sql(&fixed.sql)
        );
    }

    #[test]
    fn test_fk_set_action_column_list_does_not_block_other_fixes() {
        let sql = "CREATE TABLE child (id SERIAL PRIMARY KEY, a INT, b INT, FOREIGN KEY (a, b) REFERENCES parent (a, b) ON DELETE SET NULL (b));";
        let fixed = fix_sql(sql);
        assert!(fixed.sql.contains("GENERATED BY DEFAULT AS IDENTITY"));
        assert!(!fixed.sql.to_uppercase().contains("SERIAL"));
        assert!(fixed.sql.contains("ON DELETE SET NULL (b)"));
    }

    #[test]
    fn test_fk_set_action_column_list_restores_matching_occurrence() {
        let sql = "CREATE TABLE child (id SERIAL PRIMARY KEY, a INT, b INT, c INT, FOREIGN KEY (a, b) REFERENCES first_parent (a, b) ON DELETE SET NULL, FOREIGN KEY (a, c) REFERENCES second_parent (a, c) ON DELETE SET NULL (c));";
        let fixed = fix_sql(sql);
        let first_reference = fixed.sql.find("REFERENCES first_parent").unwrap();
        let second_reference = fixed.sql.find("REFERENCES second_parent").unwrap();
        let restored_list = fixed.sql.find("ON DELETE SET NULL (c)").unwrap();

        assert!(first_reference < second_reference);
        assert!(
            restored_list > second_reference,
            "column list restored to the wrong foreign key: {}",
            fixed.sql
        );
        assert_eq!(fixed.sql.matches("ON DELETE SET NULL (c)").count(), 1);
        assert!(
            lint_sql(&fixed.sql).is_empty(),
            "{:?}",
            lint_sql(&fixed.sql)
        );
    }

    #[test]
    fn test_fk_set_action_column_list_ignores_matching_string_literal() {
        let sql = "CREATE TABLE child (id SERIAL PRIMARY KEY, note TEXT DEFAULT 'ON DELETE SET NULL', a INT, b INT, FOREIGN KEY (a, b) REFERENCES parent (a, b) ON DELETE SET NULL (b));";
        let fixed = fix_sql(sql);
        let reference = fixed.sql.find("REFERENCES parent").unwrap();
        let restored_list = fixed.sql.rfind("ON DELETE SET NULL (b)").unwrap();

        assert!(fixed.sql.contains("DEFAULT 'ON DELETE SET NULL'"));
        assert!(
            restored_list > reference,
            "column list restored inside the string literal: {}",
            fixed.sql
        );
        assert!(
            lint_sql(&fixed.sql).is_empty(),
            "{:?}",
            lint_sql(&fixed.sql)
        );
    }

    #[test]
    fn test_supported_set_constraints_bypasses_parser_gap() {
        let supported = [
            "SET CONSTRAINTS ALL DEFERRED;",
            "SET CONSTRAINTS fk_ref IMMEDIATE;",
            "SET CONSTRAINTS public.fk_ref, \"MixedCase\" DEFERRED;",
        ];

        for sql in supported {
            assert!(lint_sql(sql).is_empty(), "expected supported SQL: {sql}");
            let fixed = fix_sql(sql);
            assert!(
                fixed.diagnostics.is_empty(),
                "expected no fix diagnostics for: {sql}"
            );
            assert_eq!(fixed.sql, format!("{sql}\n"));
        }
    }

    #[test]
    fn test_invalid_set_constraints_still_reports_parse_error() {
        let sql = "SET CONSTRAINTS ALL EVENTUALLY;";
        let diags = lint_sql(sql);
        assert!(
            diags
                .iter()
                .any(|diag| matches!(diag.rule, LintRule::ParseError)),
            "invalid constraint mode must not bypass parsing: {diags:?}"
        );
    }

    #[test]
    fn test_pgdump_create_sequence_parses() {
        // pg_dump emits CREATE SEQUENCE option clauses in an order the parser
        // must accept regardless of permutation; this pins that contract.
        let sql = "CREATE SEQUENCE public.t_id_seq AS integer \
                   START WITH 1 INCREMENT BY 1 NO MINVALUE NO MAXVALUE CACHE 1;";
        let diags = lint_sql(sql);
        assert!(
            !diags.iter().any(|d| matches!(d.rule, LintRule::ParseError)),
            "pg_dump CREATE SEQUENCE must parse without errors, got: {diags:?}"
        );
    }

    #[test]
    fn test_split_preserves_newlines() {
        let sql = "CREATE TABLE t (\n    id INT\n);\nSELECT 1;";
        let stmts = split_statements(sql).unwrap();
        assert_eq!(stmts.len(), 2);
        assert!(
            stmts[0].1.contains('\n'),
            "Statement text should preserve newlines"
        );
    }

    #[test]
    fn test_lint_without_trailing_semicolon() {
        let sql = "CREATE TABLE t (id SERIAL PRIMARY KEY)";
        let diags = lint_sql(sql);
        assert!(
            diags.iter().any(|d| d.message.contains("SERIAL")),
            "Should catch errors in SQL without trailing semicolon: {diags:?}"
        );
    }

    #[test]
    fn test_fix_sql_clean_statement_verbatim() {
        let sql = "CREATE TABLE orders (id UUID PRIMARY KEY, amount DECIMAL(10,2));";
        let result = fix_sql(sql);
        assert!(result.diagnostics.is_empty());
        assert_eq!(
            result.sql.trim(),
            sql.trim_end_matches(';').trim().to_owned() + ";"
        );
    }

    /// pg_dump's full 4-statement SERIAL expansion must collapse into a single
    /// CREATE TABLE with an inline identity column. The CREATE SEQUENCE,
    /// ALTER SEQUENCE OWNED BY, and ALTER COLUMN SET DEFAULT statements must
    /// all disappear; non-SERIAL columns and their NOT NULL stay; no
    /// ParseError is emitted (the OWNED BY line is removed before parsing it
    /// would matter); exactly one SerialSequenceIdiom diagnostic surfaces.
    #[test]
    fn test_fix_sql_collapses_pgdump_serial_idiom() {
        let sql = "\
CREATE TABLE public.t (id integer NOT NULL, x text NOT NULL);
CREATE SEQUENCE public.t_id_seq AS integer START WITH 1 INCREMENT BY 1 NO MINVALUE NO MAXVALUE CACHE 1;
ALTER SEQUENCE public.t_id_seq OWNED BY public.t.id;
ALTER TABLE ONLY public.t ALTER COLUMN id SET DEFAULT nextval('public.t_id_seq'::regclass);
";
        let result = fix_sql(sql);
        let out = &result.sql;

        let upper = out.to_uppercase();
        assert!(
            upper.contains("BIGINT") && upper.contains("GENERATED BY DEFAULT AS IDENTITY"),
            "expected inline identity column, got:\n{out}"
        );
        assert!(
            upper.contains("CACHE 1"),
            "expected CACHE 1 in identity options, got:\n{out}"
        );
        assert!(
            !out.to_lowercase().contains("nextval"),
            "nextval should be gone, got:\n{out}"
        );
        assert!(
            !out.to_uppercase().contains("CREATE SEQUENCE"),
            "CREATE SEQUENCE should be gone, got:\n{out}"
        );
        assert!(
            !out.to_uppercase().contains("ALTER SEQUENCE"),
            "ALTER SEQUENCE OWNED BY should be gone, got:\n{out}"
        );
        assert!(
            !out.to_uppercase().contains("SET DEFAULT"),
            "SET DEFAULT should be gone, got:\n{out}"
        );
        // Non-SERIAL column and its NOT NULL must survive.
        assert!(
            out.contains("x text") || out.to_uppercase().contains("X TEXT"),
            "non-SERIAL column `x text` should be preserved, got:\n{out}"
        );
        assert_eq!(
            out.matches("NOT NULL").count(),
            2,
            "exactly 2 NOT NULLs should be preserved (id + x), got:\n{out}"
        );

        assert!(
            !result
                .diagnostics
                .iter()
                .any(|d| matches!(d.rule, LintRule::ParseError)),
            "no ParseError should remain after collapse, got: {:?}",
            result.diagnostics
        );

        let idiom_diags: Vec<_> = result
            .diagnostics
            .iter()
            .filter(|d| matches!(d.rule, LintRule::SerialSequenceIdiom))
            .collect();
        assert_eq!(
            idiom_diags.len(),
            1,
            "expected exactly 1 SerialSequenceIdiom diagnostic, got: {:?}",
            result.diagnostics
        );
    }

    /// A free-standing CREATE SEQUENCE (no matching SET DEFAULT) is NOT a
    /// SERIAL idiom — leaving it alone is correct, even if other rules flag
    /// it for missing CACHE etc. Verifies we don't over-collapse.
    #[test]
    fn test_fix_sql_does_not_collapse_freestanding_sequence() {
        let sql = "\
CREATE TABLE public.t (id integer NOT NULL, x text);
CREATE SEQUENCE public.t_id_seq AS integer START WITH 1 INCREMENT BY 1 CACHE 1;
";
        let result = fix_sql(sql);
        let out = &result.sql;

        assert!(
            out.to_uppercase().contains("CREATE SEQUENCE"),
            "free-standing CREATE SEQUENCE should be kept, got:\n{out}"
        );
        assert!(
            !out.contains("GENERATED BY DEFAULT AS IDENTITY"),
            "id column must NOT become identity without a SET DEFAULT, got:\n{out}"
        );
        assert!(
            !result
                .diagnostics
                .iter()
                .any(|d| matches!(d.rule, LintRule::SerialSequenceIdiom)),
            "no SerialSequenceIdiom diagnostic should fire, got: {:?}",
            result.diagnostics
        );
    }

    /// Two independent SERIAL idioms in the same dump must each collapse
    /// independently. After fixing, each table has its own inline identity
    /// and there are exactly two SerialSequenceIdiom diagnostics.
    #[test]
    fn test_fix_sql_collapses_two_serial_idioms() {
        let sql = "\
CREATE TABLE public.a (id integer NOT NULL);
CREATE SEQUENCE public.a_id_seq AS integer START WITH 1 INCREMENT BY 1 CACHE 1;
ALTER SEQUENCE public.a_id_seq OWNED BY public.a.id;
ALTER TABLE ONLY public.a ALTER COLUMN id SET DEFAULT nextval('public.a_id_seq'::regclass);
CREATE TABLE public.b (id integer NOT NULL);
CREATE SEQUENCE public.b_id_seq AS integer START WITH 1 INCREMENT BY 1 CACHE 1;
ALTER SEQUENCE public.b_id_seq OWNED BY public.b.id;
ALTER TABLE ONLY public.b ALTER COLUMN id SET DEFAULT nextval('public.b_id_seq'::regclass);
";
        let result = fix_sql(sql);
        let out = &result.sql;

        let upper = out.to_uppercase();
        assert_eq!(
            upper.matches("GENERATED BY DEFAULT AS IDENTITY").count(),
            2,
            "expected 2 inline identity columns, got:\n{out}"
        );
        assert_eq!(
            upper.matches("BIGINT").count(),
            2,
            "expected 2 BIGINT columns, got:\n{out}"
        );
        assert!(
            !out.to_lowercase().contains("nextval"),
            "no nextval should remain, got:\n{out}"
        );
        assert!(
            !out.to_uppercase().contains("CREATE SEQUENCE"),
            "no CREATE SEQUENCE should remain, got:\n{out}"
        );
        assert!(
            !out.to_uppercase().contains("ALTER SEQUENCE"),
            "no ALTER SEQUENCE should remain, got:\n{out}"
        );

        let idiom_diags = result
            .diagnostics
            .iter()
            .filter(|d| matches!(d.rule, LintRule::SerialSequenceIdiom))
            .count();
        assert_eq!(idiom_diags, 2, "expected 2 SerialSequenceIdiom diagnostics");
    }

    /// `bigserial` expands to the same 4-statement idiom as `SERIAL`, but the
    /// CREATE TABLE column is `bigint NOT NULL` instead of `integer NOT NULL`.
    /// The collapse must still apply: the column becomes
    /// `BIGINT GENERATED BY DEFAULT AS IDENTITY (CACHE 1)`, and the
    /// CREATE SEQUENCE / OWNED BY / SET DEFAULT all disappear.
    #[test]
    fn test_fix_sql_collapses_bigserial_idiom() {
        let sql = "\
CREATE TABLE public.t (id bigint NOT NULL, x text);
CREATE SEQUENCE public.t_id_seq START WITH 1 INCREMENT BY 1 CACHE 1;
ALTER SEQUENCE public.t_id_seq OWNED BY public.t.id;
ALTER TABLE ONLY public.t ALTER COLUMN id SET DEFAULT nextval('public.t_id_seq'::regclass);
";
        let result = fix_sql(sql);
        let upper = result.sql.to_uppercase();
        assert!(
            upper.contains("BIGINT") && upper.contains("GENERATED BY DEFAULT AS IDENTITY"),
            "bigserial expansion should collapse to inline identity, got:\n{}",
            result.sql
        );
        assert!(!upper.contains("CREATE SEQUENCE"));
        assert!(!upper.contains("ALTER SEQUENCE"));
        assert!(!upper.contains("NEXTVAL"));
        assert_eq!(
            result
                .diagnostics
                .iter()
                .filter(|d| matches!(d.rule, LintRule::SerialSequenceIdiom))
                .count(),
            1
        );
    }

    /// A `SET DEFAULT nextval('external_seq')` whose CREATE SEQUENCE isn't in
    /// the input must NOT be collapsed into the CREATE TABLE (no matching
    /// sequence to fold), and the `nextval` default must be preserved verbatim.
    #[test]
    fn test_fix_sql_preserves_cross_file_sequence_default() {
        let sql = "\
CREATE TABLE public.t (id integer NOT NULL, x text);
ALTER TABLE ONLY public.t ALTER COLUMN id SET DEFAULT nextval('public.external_seq'::regclass);
";
        let result = fix_sql(sql);

        assert!(
            !result
                .diagnostics
                .iter()
                .any(|d| matches!(d.rule, LintRule::SerialSequenceIdiom)),
            "no SerialSequenceIdiom should fire without a matching CREATE SEQUENCE, got: {:?}",
            result.diagnostics
        );
        assert!(
            result.sql.to_lowercase().contains("nextval"),
            "cross-file SET DEFAULT must NOT be silently dropped, got:\n{}",
            result.sql
        );
    }

    /// A column already declared as `GENERATED BY DEFAULT AS IDENTITY` is not
    /// part of any SERIAL idiom and must pass through unchanged. Guards against
    /// the collapse pre-pass touching columns that already comply.
    #[test]
    fn test_fix_sql_leaves_inline_identity_alone() {
        let sql =
            "CREATE TABLE public.t (id BIGINT GENERATED BY DEFAULT AS IDENTITY (CACHE 1) NOT NULL, x text);";
        let result = fix_sql(sql);

        assert!(
            !result
                .diagnostics
                .iter()
                .any(|d| matches!(d.rule, LintRule::SerialSequenceIdiom)),
            "no SerialSequenceIdiom diagnostic for already-inline identity, got: {:?}",
            result.diagnostics
        );
        assert!(
            result.sql.contains("GENERATED BY DEFAULT AS IDENTITY"),
            "identity declaration should round-trip, got:\n{}",
            result.sql
        );
    }

    /// Quoted, mixed-case identifiers: pg_dump emits the SERIAL idiom verbatim
    /// for tables/columns whose names aren't lowercase-folded. The collapse must
    /// still match (sequence-name normalization strips quotes from the
    /// `nextval` literal so it agrees with the AST-derived identifier).
    #[test]
    fn test_fix_sql_collapses_idiom_with_quoted_mixed_case_identifiers() {
        let sql = "\
CREATE TABLE public.\"T\" (\"Id\" integer NOT NULL, x text);
CREATE SEQUENCE public.\"T_Id_seq\" AS integer START WITH 1 INCREMENT BY 1 CACHE 1;
ALTER SEQUENCE public.\"T_Id_seq\" OWNED BY public.\"T\".\"Id\";
ALTER TABLE ONLY public.\"T\" ALTER COLUMN \"Id\" SET DEFAULT nextval('public.\"T_Id_seq\"'::regclass);
";
        let result = fix_sql(sql);
        let upper = result.sql.to_uppercase();
        assert!(
            upper.contains("BIGINT") && upper.contains("GENERATED BY DEFAULT AS IDENTITY"),
            "quoted mixed-case idiom must collapse to inline identity, got:\n{}",
            result.sql
        );
        assert!(
            !upper.contains("CREATE SEQUENCE"),
            "CREATE SEQUENCE for quoted name should be gone, got:\n{}",
            result.sql
        );
        assert!(
            !upper.contains("ALTER SEQUENCE"),
            "ALTER SEQUENCE OWNED BY for quoted name should be gone, got:\n{}",
            result.sql
        );
        assert!(
            !result.sql.to_lowercase().contains("nextval"),
            "nextval should be gone, got:\n{}",
            result.sql
        );
        assert_eq!(
            result
                .diagnostics
                .iter()
                .filter(|d| matches!(d.rule, LintRule::SerialSequenceIdiom))
                .count(),
            1
        );
    }

    /// Multi-op ALTER TABLE bundling SET DEFAULT with sibling operations (here:
    /// ADD CONSTRAINT … PRIMARY KEY) must NOT be collapsed into the SERIAL
    /// idiom, and the unrelated PRIMARY KEY must survive untouched.
    #[test]
    fn test_fix_sql_serial_idiom_preserves_unrelated_alter_ops() {
        let sql = "\
CREATE TABLE public.t (id integer NOT NULL, x text);
CREATE SEQUENCE public.t_id_seq CACHE 1;
ALTER TABLE ONLY public.t \
ALTER COLUMN id SET DEFAULT nextval('public.t_id_seq'::regclass), \
ADD CONSTRAINT t_pkey PRIMARY KEY (id);
";
        let result = fix_sql(sql);

        assert!(
            !result
                .diagnostics
                .iter()
                .any(|d| matches!(d.rule, LintRule::SerialSequenceIdiom)),
            "no SerialSequenceIdiom should fire when SET DEFAULT shares an ALTER \
             TABLE with other operations, got: {:?}",
            result.diagnostics
        );
        assert!(
            result.sql.to_uppercase().contains("PRIMARY KEY"),
            "the unrelated ADD CONSTRAINT t_pkey PRIMARY KEY must survive, got:\n{}",
            result.sql
        );
    }

    /// A column already declared `GENERATED ALWAYS AS IDENTITY` plus an erroneous
    /// pg_dump-shaped SET DEFAULT trio: collapse must NOT produce a CREATE TABLE
    /// with two `GENERATED ... AS IDENTITY` clauses (which is invalid SQL). The
    /// existing identity option is replaced by the canonical
    /// `GENERATED BY DEFAULT AS IDENTITY (CACHE 1)` shape.
    #[test]
    fn test_fix_sql_serial_idiom_does_not_double_identity() {
        let sql = "\
CREATE TABLE public.t (id BIGINT GENERATED ALWAYS AS IDENTITY (CACHE 1) NOT NULL, x text);
CREATE SEQUENCE public.t_id_seq CACHE 1;
ALTER SEQUENCE public.t_id_seq OWNED BY public.t.id;
ALTER TABLE ONLY public.t ALTER COLUMN id SET DEFAULT nextval('public.t_id_seq'::regclass);
";
        let result = fix_sql(sql);
        let upper = result.sql.to_uppercase();
        assert_eq!(
            upper.matches("GENERATED").count(),
            1,
            "exactly one identity clause must remain, got:\n{}",
            result.sql
        );
        assert!(
            upper.contains("GENERATED BY DEFAULT AS IDENTITY"),
            "the surviving identity must be the canonical BY DEFAULT shape, got:\n{}",
            result.sql
        );
    }

    /// Inline-`PRIMARY KEY` column option must survive the SERIAL→identity
    /// rewrite (only DEFAULT and existing identity options are dropped).
    #[test]
    fn test_fix_sql_serial_idiom_preserves_primary_key_option() {
        let sql = "\
CREATE TABLE public.t (id integer NOT NULL PRIMARY KEY, x text);
CREATE SEQUENCE public.t_id_seq CACHE 1;
ALTER SEQUENCE public.t_id_seq OWNED BY public.t.id;
ALTER TABLE ONLY public.t ALTER COLUMN id SET DEFAULT nextval('public.t_id_seq'::regclass);
";
        let result = fix_sql(sql);
        let upper = result.sql.to_uppercase();
        assert!(
            upper.contains("PRIMARY KEY"),
            "PRIMARY KEY column option must survive, got:\n{}",
            result.sql
        );
        assert!(
            upper.contains("GENERATED BY DEFAULT AS IDENTITY"),
            "the column must still become an identity, got:\n{}",
            result.sql
        );
    }

    /// The `FixedWithWarning` warning text is the entire reason this rule
    /// emits a warning instead of a plain `Fixed` — it tells the user the
    /// identity counter was NOT advanced past existing data, so backfill needs
    /// a manual reset. Pin the substring so a future refactor can't silently
    /// reword the warning into something less actionable.
    #[test]
    fn test_fix_sql_serial_idiom_warning_mentions_counter_reset() {
        let sql = "\
CREATE TABLE public.t (id integer NOT NULL);
CREATE SEQUENCE public.t_id_seq CACHE 1;
ALTER SEQUENCE public.t_id_seq OWNED BY public.t.id;
ALTER TABLE ONLY public.t ALTER COLUMN id SET DEFAULT nextval('public.t_id_seq'::regclass);
";
        let result = fix_sql(sql);
        let warning = result
            .diagnostics
            .iter()
            .find_map(|d| match (&d.rule, &d.fix_result) {
                (LintRule::SerialSequenceIdiom, FixResult::FixedWithWarning(s)) => Some(s.clone()),
                _ => None,
            })
            .expect("expected a SerialSequenceIdiom FixedWithWarning diagnostic");
        let lower = warning.to_lowercase();
        assert!(
            lower.contains("counter") && lower.contains("reset"),
            "warning must tell the user to reset the identity counter, got: {warning}"
        );
    }
}
