//! Core linting engine: parse SQL → walk AST → apply rules → collect diagnostics.

use regex::Regex;
use sqlparser::{
    dialect::PostgreSqlDialect,
    parser::Parser,
    tokenizer::{Token, Tokenizer},
};

use crate::rules;

/// A single compatibility issue found in the input SQL.
///
/// Returned by [`lint_sql`] and consumed by both the CLI (for human-readable output)
/// and the library crate (for programmatic integration, e.g. in MCP servers).
#[derive(Debug, Clone, Default)]
pub struct Diagnostic {
    pub line: usize,
    pub statement: String,
    pub message: String,
    pub suggestion: String,
    /// `true` = ERROR (will fail on DSQL), `false` = WARNING (best practice).
    pub is_error: bool,
}

/// Split SQL input into `(line_number, statement_text)` pairs on `;`.
///
/// Uses the tokenizer instead of naive string splitting so semicolons inside
/// quoted strings or comments don't produce false boundaries.
fn split_statements(input: &str) -> Vec<(usize, String)> {
    let dialect = PostgreSqlDialect {};
    let Ok(all_tokens) = Tokenizer::new(&dialect, input).tokenize_with_location() else {
        return vec![(1, input.to_string())];
    };

    let mut results = Vec::new();
    let mut stmt_tokens: Vec<String> = Vec::new();
    let mut stmt_line = 0u64;

    for twl in &all_tokens {
        match &twl.token {
            Token::Whitespace(_) | Token::EOF => continue,
            Token::SemiColon => {
                if !stmt_tokens.is_empty() {
                    results.push((stmt_line as usize, stmt_tokens.join(" ")));
                    stmt_tokens.clear();
                }
                stmt_line = 0;
            }
            tok => {
                if stmt_line == 0 {
                    stmt_line = twl.span.start.line;
                }
                stmt_tokens.push(tok.to_string());
            }
        }
    }

    if !stmt_tokens.is_empty() {
        results.push((stmt_line as usize, stmt_tokens.join(" ")));
    }

    results
}

pub fn lint_sql(sql: &str) -> Vec<Diagnostic> {
    let dialect = PostgreSqlDialect {};

    // ASYNC is a DSQL-specific keyword that sqlparser doesn't recognise.
    // Strip it before parsing so the rest of the statement can be analysed,
    // then compare against the original text to detect its absence.
    let re = Regex::new(r"(?i)(CREATE\s+(UNIQUE\s+)?INDEX)\s+ASYNC\b").unwrap();
    let cleaned = re.replace_all(sql, "$1");

    let stmts = split_statements(&cleaned);
    let original_stmts = split_statements(sql);

    let mut diagnostics = Vec::new();

    for (i, (line_num, stmt_text)) in stmts.iter().enumerate() {
        let trimmed = stmt_text.trim();
        if trimmed.is_empty() {
            continue;
        }

        let parsed = match Parser::parse_sql(&dialect, trimmed) {
            Ok(p) => p,
            Err(e) => {
                diagnostics.push(Diagnostic {
                    line: *line_num,
                    statement: trimmed.to_string(),
                    message: format!("Failed to parse SQL: {e}"),
                    suggestion: "Fix the SQL syntax and try again.".to_string(),
                    is_error: true,
                });
                continue;
            }
        };

        let original_text = original_stmts
            .get(i)
            .map(|(_, text)| text.as_str())
            .unwrap_or(trimmed);

        for stmt in &parsed {
            let mut stmt_diags = Vec::new();
            rules::check_statement(stmt, original_text, &mut stmt_diags);

            // Rules report line numbers relative to their statement;
            // translate to absolute line numbers in the original input.
            for d in &mut stmt_diags {
                d.line = line_num + d.line - 1;
                d.statement = original_text.to_string();
            }
            diagnostics.extend(stmt_diags);
        }
    }
    diagnostics
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_valid_create_table_no_errors() {
        let sql = "CREATE TABLE orders (id UUID PRIMARY KEY, tenant_id VARCHAR(255) NOT NULL, amount DECIMAL(10,2));";
        let diags = lint_sql(sql);
        let errors: Vec<_> = diags.iter().filter(|d| d.is_error).collect();
        assert!(errors.is_empty(), "Expected no errors, got: {:?}", errors);
    }

    #[test]
    fn test_parse_error_returns_diagnostic() {
        let sql = "NOT VALID SQL AT ALL ???";
        let diags = lint_sql(sql);
        assert!(!diags.is_empty());
        assert!(diags[0].message.contains("Failed to parse SQL"));
    }
}
