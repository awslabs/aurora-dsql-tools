//! Core linting engine: parse SQL → walk AST → apply rules → collect diagnostics.

use sqlparser::dialect::PostgreSqlDialect;
use sqlparser::parser::Parser;

use crate::rules;

/// A single diagnostic produced by a lint rule.
#[derive(Debug, Clone)]
pub struct Diagnostic {
    pub line: usize,
    pub rule_id: String,
    pub message: String,
    pub suggestion: String,
    pub is_error: bool,
}

/// Lint a SQL string and return all diagnostics.
pub fn lint_sql(sql: &str) -> Vec<Diagnostic> {
    let dialect = PostgreSqlDialect {};
    let statements = match Parser::parse_sql(&dialect, sql) {
        Ok(stmts) => stmts,
        Err(e) => {
            return vec![Diagnostic {
                line: 1,
                rule_id: "E000".to_string(),
                message: format!("Failed to parse SQL: {e}"),
                suggestion: "Fix the SQL syntax and try again.".to_string(),
                is_error: true,
            }];
        }
    };

    let mut diagnostics = Vec::new();
    for stmt in &statements {
        rules::check_statement(stmt, sql, &mut diagnostics);
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
        assert_eq!(diags[0].rule_id, "E000");
    }
}
