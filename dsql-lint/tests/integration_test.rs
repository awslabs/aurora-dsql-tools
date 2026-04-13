use dsql_lint::lint::lint_sql;

#[test]
fn test_sample_migration_file() {
    let sql = include_str!("fixtures/sample_migration.sql");
    let diags = lint_sql(sql);

    let error_ids: Vec<&str> = diags.iter()
        .filter(|d| d.is_error)
        .map(|d| d.rule_id.as_str())
        .collect();

    assert!(error_ids.contains(&"E003"), "Missing E003 (SERIAL)");
    assert!(error_ids.contains(&"E001"), "Missing E001 (FOREIGN KEY)");
    assert!(error_ids.contains(&"E006"), "Missing E006 (JSON)");
    assert!(error_ids.contains(&"E007"), "Missing E007 (TRUNCATE)");
    assert!(error_ids.contains(&"E008"), "Missing E008 (TEMP TABLE)");
    assert!(error_ids.contains(&"E009"), "Missing E009 (Array type)");

    // W001 for settings table (and others missing tenant_id like users, scratch)
    let warnings: Vec<_> = diags.iter()
        .filter(|d| !d.is_error && d.rule_id == "W001")
        .collect();
    assert!(!warnings.is_empty(), "Missing W001 (tenant_id)");

    // Valid DML should not produce errors
    let dml_errors: Vec<_> = diags.iter()
        .filter(|d| d.message.contains("INSERT") || d.message.contains("SELECT") || d.message.contains("DELETE"))
        .collect();
    assert!(dml_errors.is_empty(), "DML should not produce errors: {:?}", dml_errors);
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
    assert!(errors.is_empty(), "Clean SQL should have no errors: {:?}", errors);
}
