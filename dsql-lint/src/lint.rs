//! Core linting engine: parse SQL -> walk AST -> apply rules -> collect diagnostics.

use sqlparser::{
    ast::Statement,
    dialect::PostgreSqlDialect,
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, strum::EnumIter)]
#[cfg_attr(
    feature = "serde",
    derive(serde::Serialize),
    serde(rename_all = "snake_case")
)]
pub enum LintRule {
    SerialType,
    JsonType,
    ArrayType,
    ForeignKey,
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
    IndexExpression,
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
    AtUnsupportedDropColumn,
    AtUnsupportedAlterColumnSetType,
    AtUnsupportedAlterColumnSetNotNull,
    AtUnsupportedAlterColumnDropNotNull,
    AtUnsupportedAlterColumnSetDefault,
    AtUnsupportedAlterColumnDropDefault,
    AtUnsupportedAlterColumnAddGenerated,
    AtUnsupportedAddCheck,
    AtUnsupportedAddPrimaryKey,
    AtUnsupportedAddUnique,
    AtUnsupportedDropConstraint,
    AtUnsupportedPrimaryKeyUsingIndex,
    AtUnsupportedUniqueUsingIndex,
    AtUnsupportedRowLevelSecurity,
    AtUnsupportedTrigger,
    AtUnsupportedReplicaIdentity,
    AtUnsupportedValidateConstraint,
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
    let dialect = PostgreSqlDialect {};
    let all_tokens = Tokenizer::new(&dialect, input)
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

/// Rules take `&mut` and may mutate the AST — kept intentionally so each rule is a single code path for both lint and fix, avoiding duplicated logic that can drift.
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

    for (line_num, stmt_text) in &stmts {
        if stmt_text.trim().is_empty() {
            continue;
        }

        let mut parsed = match Parser::parse_sql(&dialect, stmt_text) {
            Ok(p) => p,
            Err(e) => {
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
    let dialect = PostgreSqlDialect {};
    let mut all_diagnostics = Vec::new();
    let mut fixed_parts: Vec<(usize, String)> = Vec::new();

    let mut stmts = match split_statements(sql) {
        Ok(s) => s,
        Err(e) => {
            all_diagnostics.push(Diagnostic {
                rule: LintRule::ParseError,
                line: 1,
                statement: String::new(),
                message: format!("Failed to tokenize SQL: {e}"),
                suggestion: "Fix the SQL syntax and try again.".to_string(),
                fix_result: FixResult::Unfixable,
            });
            return FixOutput {
                sql: sql.to_string(),
                diagnostics: all_diagnostics,
            };
        }
    };

    // Pre-passes: collapse multi-statement idioms BEFORE the per-statement
    // loop, so the loop never emits Unfixable diagnostics on statements
    // we just folded away (or ParseError on the unparseable
    // `ALTER SEQUENCE ... OWNED BY` line that the SERIAL idiom drops).
    rules::serial_idiom::fix_serial_idioms(&mut stmts, &mut all_diagnostics);
    rules::constraint_collapse::fix_alter_add_unique(&mut stmts, &mut all_diagnostics);
    rules::constraint_collapse::fix_alter_add_primary_key(&mut stmts, &mut all_diagnostics);

    for (line_num, stmt_text) in &stmts {
        if stmt_text.trim().is_empty() {
            fixed_parts.push((*line_num, stmt_text.to_string()));
            continue;
        }

        let mut parsed = match Parser::parse_sql(&dialect, stmt_text) {
            Ok(p) => p,
            Err(e) => {
                fixed_parts.push((*line_num, stmt_text.trim_end_matches(';').to_string()));
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
            let fixed = parsed
                .iter()
                .map(|s| format!("{:#}", s))
                .collect::<Vec<_>>()
                .join(";\n");
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

    /// A `SET DEFAULT nextval('external_seq')` whose CREATE SEQUENCE lives in a
    /// different file (or wasn't dumped) must NOT be silently collapsed —
    /// dropping the SET DEFAULT without a replacement would lose the column's
    /// auto-increment behavior. The collapse skips it; the existing
    /// per-statement rule (`AtUnsupportedAlterColumnSetDefault`) still flags
    /// the SET DEFAULT as Unfixable, so the user is told.
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
        assert!(
            result.diagnostics.iter().any(|d| matches!(
                d.rule,
                LintRule::AtUnsupportedAlterColumnSetDefault
            ) && matches!(d.fix_result, FixResult::Unfixable)),
            "SET DEFAULT should be flagged Unfixable so the user notices, got: {:?}",
            result.diagnostics
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
    /// ADD CONSTRAINT … PRIMARY KEY) must NOT collapse — the per-statement
    /// `AtUnsupportedAlterColumnSetDefault` rule still flags the SET DEFAULT
    /// Unfixable so the user is told, but the PRIMARY KEY survives untouched.
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
        assert!(
            result.diagnostics.iter().any(|d| matches!(
                d.rule,
                LintRule::AtUnsupportedAlterColumnSetDefault
            ) && matches!(d.fix_result, FixResult::Unfixable)),
            "SET DEFAULT should be flagged Unfixable, got: {:?}",
            result.diagnostics
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
