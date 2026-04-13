use sqlparser::ast::{AlterTableOperation, ColumnOption, Statement};

use crate::lint::Diagnostic;

fn warning(line: usize, message: impl Into<String>, suggestion: impl Into<String>) -> Diagnostic {
    Diagnostic {
        line,
        statement: String::new(),
        message: message.into(),
        suggestion: suggestion.into(),
        is_error: false,
    }
}

pub fn check(stmt: &Statement, diagnostics: &mut Vec<Diagnostic>) {
    check_w001(stmt, diagnostics);
    check_w002(stmt, diagnostics);
}

/// CREATE TABLE missing `tenant_id` column.
fn check_w001(stmt: &Statement, diagnostics: &mut Vec<Diagnostic>) {
    let Statement::CreateTable(ct) = stmt else { return };

    let has_tenant_id = ct
        .columns
        .iter()
        .any(|col| col.name.value.to_lowercase() == "tenant_id");

    if !has_tenant_id {
        diagnostics.push(warning(
            1,
            "CREATE TABLE is missing a tenant_id column.",
            "Add tenant_id VARCHAR(255) NOT NULL for multi-tenant isolation",
        ));
    }
}

/// ALTER TABLE ADD COLUMN with inline DEFAULT or NOT NULL.
fn check_w002(stmt: &Statement, diagnostics: &mut Vec<Diagnostic>) {
    let Statement::AlterTable(alter_table) = stmt else { return };

    for op in &alter_table.operations {
        if let AlterTableOperation::AddColumn { column_def, .. } = op {
            let has_default_or_not_null = column_def.options.iter().any(|opt| {
                matches!(opt.option, ColumnOption::Default(_) | ColumnOption::NotNull)
            });
            if has_default_or_not_null {
                diagnostics.push(warning(
                    1,
                    format!("ADD COLUMN '{}' has inline DEFAULT or NOT NULL constraint.", column_def.name.value),
                    "Split into steps: ADD COLUMN, UPDATE, ALTER COLUMN",
                ));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::lint::lint_sql;

    #[test]
    fn test_w001_missing_tenant_id() {
        let sql = "CREATE TABLE orders (id INT, amount DECIMAL);";
        let diags = lint_sql(sql);
        assert!(
            diags.iter().any(|d| d.message.contains("tenant_id")),
            "Expected tenant_id warning, got: {:?}",
            diags
        );
    }

    #[test]
    fn test_w001_has_tenant_id() {
        let sql = "CREATE TABLE orders (id INT, tenant_id VARCHAR(255) NOT NULL, amount DECIMAL);";
        let diags = lint_sql(sql);
        assert!(
            !diags.iter().any(|d| d.message.contains("tenant_id")),
            "Expected no tenant_id warning, got: {:?}",
            diags
        );
    }

    #[test]
    fn test_w002_add_column_with_default() {
        let sql = "ALTER TABLE orders ADD COLUMN status VARCHAR(50) DEFAULT 'pending';";
        let diags = lint_sql(sql);
        assert!(
            diags.iter().any(|d| d.message.contains("ADD COLUMN")),
            "Expected ADD COLUMN warning, got: {:?}",
            diags
        );
    }

    #[test]
    fn test_w002_add_column_with_not_null() {
        let sql = "ALTER TABLE orders ADD COLUMN status VARCHAR(50) NOT NULL;";
        let diags = lint_sql(sql);
        assert!(
            diags.iter().any(|d| d.message.contains("ADD COLUMN")),
            "Expected ADD COLUMN warning, got: {:?}",
            diags
        );
    }

    #[test]
    fn test_w002_add_column_plain_is_ok() {
        let sql = "ALTER TABLE orders ADD COLUMN status VARCHAR(50);";
        let diags = lint_sql(sql);
        assert!(
            !diags.iter().any(|d| d.message.contains("ADD COLUMN")),
            "Expected no ADD COLUMN warning, got: {:?}",
            diags
        );
    }
}
