use sqlparser::ast::{AlterTableOperation, ColumnOption, Statement};

use crate::lint::{Diagnostic, Severity};

use super::find_line;

fn warning(line: usize, message: impl Into<String>, suggestion: impl Into<String>) -> Diagnostic {
    Diagnostic {
        line,
        statement: String::new(),
        message: message.into(),
        suggestion: suggestion.into(),
        severity: Severity::Warning,
    }
}

pub(crate) fn check(stmt: &Statement, raw_sql: &str, diagnostics: &mut Vec<Diagnostic>) {
    check_add_column_constraints(stmt, raw_sql, diagnostics);
}

/// ALTER TABLE ADD COLUMN with inline DEFAULT or NOT NULL.
fn check_add_column_constraints(
    stmt: &Statement,
    raw_sql: &str,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let Statement::AlterTable(alter_table) = stmt else {
        return;
    };

    for op in &alter_table.operations {
        if let AlterTableOperation::AddColumn { column_def, .. } = op {
            let has_default_or_not_null = column_def
                .options
                .iter()
                .any(|opt| matches!(opt.option, ColumnOption::Default(_) | ColumnOption::NotNull));
            if has_default_or_not_null {
                let line = find_line(raw_sql, &column_def.name.value.to_lowercase());
                diagnostics.push(warning(
                    line,
                    format!(
                        "ADD COLUMN '{}' has inline DEFAULT or NOT NULL constraint.",
                        column_def.name.value
                    ),
                    "Split into steps: ADD COLUMN, UPDATE, ALTER COLUMN",
                ));
            }
        }
    }
}
