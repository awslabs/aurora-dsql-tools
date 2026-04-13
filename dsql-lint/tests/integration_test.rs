use dsql_lint::lint::lint_sql;

#[test]
fn test_sample_migration_file() {
    let sql = include_str!("fixtures/sample_migration.sql");
    let diags = lint_sql(sql);

    let errors: Vec<_> = diags.iter().filter(|d| d.is_error).collect();
    assert!(
        errors.iter().any(|d| d.message.contains("SERIAL")),
        "Missing SERIAL error"
    );
    assert!(
        errors.iter().any(|d| d.message.contains("FOREIGN KEY")),
        "Missing FOREIGN KEY error"
    );
    assert!(
        errors.iter().any(|d| d.message.contains("JSON")),
        "Missing JSON error"
    );
    assert!(
        errors.iter().any(|d| d.message.contains("TRUNCATE")),
        "Missing TRUNCATE error"
    );
    assert!(
        errors.iter().any(|d| d.message.contains("TEMPORARY")),
        "Missing TEMP TABLE error"
    );
    assert!(
        errors.iter().any(|d| d.message.contains("array")),
        "Missing array error"
    );

    // W001 for settings table (and others missing tenant_id like users, scratch)
    let warnings: Vec<_> = diags
        .iter()
        .filter(|d| !d.is_error && d.message.contains("tenant_id"))
        .collect();
    assert!(!warnings.is_empty(), "Missing tenant_id warning");

    // Valid DML should not produce errors
    let dml_errors: Vec<_> = diags
        .iter()
        .filter(|d| {
            d.message.contains("INSERT")
                || d.message.contains("SELECT")
                || d.message.contains("DELETE")
        })
        .collect();
    assert!(
        dml_errors.is_empty(),
        "DML should not produce errors: {:?}",
        dml_errors
    );
}

#[test]
fn test_clean_file_exits_zero_errors() {
    let sql = r#"
        CREATE TABLE clean (
            id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
            tenant_id VARCHAR(255) NOT NULL,
            name VARCHAR(255)
        );

        INSERT INTO clean (tenant_id, name) VALUES ('acme', 'Alice');
    "#;

    let diags = lint_sql(sql);
    let errors: Vec<_> = diags.iter().filter(|d| d.is_error).collect();
    assert!(
        errors.is_empty(),
        "Clean SQL should have no errors: {:?}",
        errors
    );
}
