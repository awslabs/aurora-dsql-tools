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
pub enum FixResult {
    Fixed(String),
    FixedWithWarning(String),
    Unfixable,
}

/// A single compatibility issue found in the input SQL.
///
/// Returned by [`lint_sql`] and consumed by both the CLI (for human-readable output)
/// and the library crate (for programmatic integration, e.g. in MCP servers).
#[derive(Debug, Clone)]
pub struct Diagnostic {
    pub line: usize,
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
fn split_statements(input: &str) -> Result<Vec<(usize, String)>, String> {
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
            | Statement::Drop { .. }
            | Statement::Truncate(_)
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
        line,
        statement: begin_text.to_string(),
        message: format!(
            "Transaction contains {ddl_count} DDL statements. DSQL supports only one DDL statement per transaction."
        ),
        suggestion: "Split into separate transactions: wrap each DDL statement in its own BEGIN/COMMIT block.".to_string(),
        fix_result,
    }
}

/// Cross-statement pass: detect transaction blocks (BEGIN … COMMIT) with more
/// than one DDL statement. DSQL allows only one DDL per transaction.
fn check_ddl_transactions(stmts: &[(usize, String)], diagnostics: &mut Vec<Diagnostic>) {
    let dialect = PostgreSqlDialect {};
    let mut in_txn = false;
    let mut txn_begin_line: usize = 0;
    let mut txn_begin_text = String::new();
    let mut ddl_count: usize = 0;

    for (line_num, stmt_text) in stmts {
        let parsed = match Parser::parse_sql(&dialect, stmt_text.trim()) {
            Ok(p) => p,
            Err(e) => {
                if in_txn {
                    diagnostics.push(Diagnostic {
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
            if is_begin(stmt) {
                if !in_txn {
                    in_txn = true;
                    txn_begin_line = *line_num;
                    txn_begin_text = stmt_text.to_string();
                    ddl_count = 0;
                }
                // Nested BEGIN inside an open transaction is a no-op in PostgreSQL/DSQL —
                // don't reset the DDL count.
            } else if is_txn_end(stmt) {
                if in_txn && ddl_count > 1 && is_commit(stmt) {
                    diagnostics.push(multi_ddl_txn_diagnostic(
                        txn_begin_line,
                        ddl_count,
                        &txn_begin_text,
                        FixResult::Unfixable,
                    ));
                }
                in_txn = false;
            } else if in_txn && is_ddl(stmt) {
                ddl_count += 1;
            }
        }
    }
}

/// Fix pass: split transaction blocks containing multiple DDL statements so
/// each DDL gets its own BEGIN/COMMIT wrapper.
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
        let mut commit_idx = None;

        let mut nested_begin_indices = Vec::new();
        'txn: for (j, (line, text)) in parts.iter().enumerate().skip(begin_idx + 1) {
            let p = match Parser::parse_sql(&dialect, text.trim()) {
                Ok(p) => p,
                Err(e) => {
                    diagnostics.push(Diagnostic {
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
            }
        }

        let commit_idx = match commit_idx {
            Some(idx) => idx,
            None => {
                i += 1;
                continue;
            }
        };

        if ddl_indices.len() <= 1 {
            i = commit_idx + 1;
            continue;
        }

        let ddl_count = ddl_indices.len();
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

        diagnostics.push(multi_ddl_txn_diagnostic(
            begin_line,
            ddl_count,
            &begin_text,
            FixResult::FixedWithWarning(
                "Split multi-DDL transaction into individual BEGIN/COMMIT blocks".to_string(),
            ),
        ));

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
                line: 1,
                statement: String::new(),
                message: format!("Failed to tokenize SQL: {e}"),
                suggestion: "Fix the SQL syntax and try again.".to_string(),
                fix_result: FixResult::Unfixable,
            });
            return diagnostics;
        }
    };

    for (line_num, stmt_text) in &stmts {
        if stmt_text.trim().is_empty() {
            continue;
        }

        let mut parsed = match Parser::parse_sql(&dialect, stmt_text) {
            Ok(p) => p,
            Err(e) => {
                diagnostics.push(Diagnostic {
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

    let stmts = match split_statements(sql) {
        Ok(s) => s,
        Err(e) => {
            all_diagnostics.push(Diagnostic {
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
}
