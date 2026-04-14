use dsql_lint::lint::{lint_sql, Severity};

#[test]
fn test_sample_migration_file() {
    let sql = include_str!("fixtures/sample_migration.sql");
    let diags = lint_sql(sql);

    let errors: Vec<_> = diags
        .iter()
        .filter(|d| d.severity == Severity::Error)
        .collect();
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
    assert!(
        errors.iter().any(|d| d.message.contains("ASYNC")),
        "Missing ASYNC error"
    );

    // settings table (and others missing tenant_id like users, scratch)
    let warnings: Vec<_> = diags
        .iter()
        .filter(|d| d.severity == Severity::Warning && d.message.contains("tenant_id"))
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
    let errors: Vec<_> = diags
        .iter()
        .filter(|d| d.severity == Severity::Error)
        .collect();
    assert!(
        errors.is_empty(),
        "Clean SQL should have no errors: {:?}",
        errors
    );
}

/// Realistic DSQL-compatible SQL that should NOT trigger any errors.
/// Each statement targets a specific rule to verify it doesn't false-positive.
#[test]
fn test_no_false_positives() {
    let sql = r#"
        -- Column named 'serial_number' should not trigger SERIAL rule
        CREATE TABLE products (
            id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
            tenant_id VARCHAR(255) NOT NULL,
            serial_number VARCHAR(100) NOT NULL
        );

        -- Column named 'json_data' with TEXT type should not trigger JSON rule
        CREATE TABLE events (
            id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
            tenant_id VARCHAR(255) NOT NULL,
            json_data TEXT NOT NULL
        );

        -- Table named 'temporary_cache' should not trigger TEMP table rule
        CREATE TABLE temporary_cache (
            id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
            tenant_id VARCHAR(255) NOT NULL,
            payload TEXT
        );

        -- Column named 'tags' with TEXT type should not trigger array rule
        CREATE TABLE items (
            id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
            tenant_id VARCHAR(255) NOT NULL,
            tags TEXT
        );

        -- DELETE FROM should not trigger TRUNCATE rule
        DELETE FROM events WHERE tenant_id = 'old';

        -- CREATE VIEW should not trigger CREATE TABLE rules
        CREATE VIEW active_products AS SELECT * FROM products WHERE tenant_id = 'acme';

        -- ALTER TABLE DROP COLUMN should not trigger ADD COLUMN warning
        ALTER TABLE products DROP COLUMN serial_number;

        -- ALTER TABLE ADD COLUMN without DEFAULT or NOT NULL is fine
        ALTER TABLE products ADD COLUMN description TEXT;

        -- GENERATED ALWAYS AS IDENTITY should not trigger SERIAL rule
        CREATE TABLE counters (
            id INTEGER GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
            tenant_id VARCHAR(255) NOT NULL,
            count INT DEFAULT 0
        );

        -- CREATE FUNCTION with LANGUAGE SQL is allowed in DSQL
        CREATE FUNCTION add(a INT, b INT) RETURNS INT AS $$ SELECT a + b; $$ LANGUAGE SQL;

        -- CREATE INDEX ASYNC is valid DSQL
        CREATE INDEX ASYNC idx_events_tenant ON events(tenant_id);

        -- Column named 'partition_key' should not trigger PARTITION BY rule
        CREATE TABLE sharded (
            id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
            tenant_id VARCHAR(255) NOT NULL,
            partition_key VARCHAR(50)
        );

        -- DML with keywords in string literals should not trigger rules
        INSERT INTO events (id, tenant_id, json_data)
        VALUES (gen_random_uuid(), 'acme', 'TRUNCATE TABLE foo; CREATE TRIGGER bar');
    "#;

    let diags = lint_sql(sql);
    let errors: Vec<_> = diags
        .iter()
        .filter(|d| d.severity == Severity::Error)
        .collect();
    assert!(
        errors.is_empty(),
        "Expected no false positive errors, got: {:?}",
        errors
    );
}
