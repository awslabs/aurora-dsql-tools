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
    diagnostics
}

pub struct FixOutput {
    pub sql: String,
    pub diagnostics: Vec<Diagnostic>,
}

pub fn fix_sql(sql: &str) -> FixOutput {
    let dialect = PostgreSqlDialect {};
    let mut all_diagnostics = Vec::new();
    let mut fixed_parts: Vec<String> = Vec::new();

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
            fixed_parts.push(stmt_text.to_string());
            continue;
        }

        let mut parsed = match Parser::parse_sql(&dialect, stmt_text) {
            Ok(p) => p,
            Err(e) => {
                fixed_parts.push(stmt_text.trim_end_matches(';').to_string());
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
            fixed_parts.push(fixed);
        } else {
            fixed_parts.push(stmt_text.trim_end_matches(';').to_string());
        }

        for d in &mut stmt_diags {
            d.line = line_num + d.line - 1;
            d.statement = stmt_text.to_string();
        }
        all_diagnostics.extend(stmt_diags);
    }

    let mut sql = fixed_parts.join(";\n\n");
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
