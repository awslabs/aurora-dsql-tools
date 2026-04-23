//! Integration tests for dsql-lint.
//!
//! Matrix-driven test groups:
//!   1. Supported types — every DSQL type must lint clean
//!   2. Error detection — every unsupported pattern must be caught
//!   3. Suggestion validity — every suggested replacement must itself be valid
//!   4. False positives — valid SQL must not trigger errors
//!   5. Additional error detection
//!   6. Fixtures — realistic multi-statement SQL files

mod common;

use dsql_lint::lint_sql;

// ═══════════════════════════════════════════════════════════════════════
// 1. SUPPORTED TYPES MATRIX
// ═══════════════════════════════════════════════════════════════════════
// Shared list lives in tests/common/mod.rs. If a new lint rule
// false-positives on a valid type, this catches it.

#[test]
fn supported_types_produce_zero_errors() {
    for (label, col_type) in common::SUPPORTED_TYPES {
        let sql = format!("CREATE TABLE _type_test (col {col_type});");
        let diags = lint_sql(&sql);
        assert!(
            diags.is_empty(),
            "[{label}] Supported type `{col_type}` triggered errors:\n  SQL: {sql}\n  Errors: {diags:?}"
        );
    }
}

// ═══════════════════════════════════════════════════════════════════════
// 2. ERROR DETECTION MATRIX
// ═══════════════════════════════════════════════════════════════════════
// Every unsupported pattern the linter should catch, tested through the
// full lint_sql pipeline (statement splitting + parsing + all rules).
// (category, sql, expected message substring)

const ERROR_CASES: &[(&str, &str, &str)] = &[
    // SERIAL variants — all aliases
    (
        "serial",
        "CREATE TABLE t (id SERIAL PRIMARY KEY);",
        "SERIAL",
    ),
    (
        "serial",
        "CREATE TABLE t (id SERIAL4 PRIMARY KEY);",
        "SERIAL4",
    ),
    (
        "serial",
        "CREATE TABLE t (id BIGSERIAL PRIMARY KEY);",
        "BIGSERIAL",
    ),
    (
        "serial",
        "CREATE TABLE t (id SERIAL8 PRIMARY KEY);",
        "SERIAL8",
    ),
    (
        "serial",
        "CREATE TABLE t (id SMALLSERIAL PRIMARY KEY);",
        "SMALLSERIAL",
    ),
    (
        "serial",
        "CREATE TABLE t (id SERIAL2 PRIMARY KEY);",
        "SERIAL2",
    ),
    // JSON / JSONB
    ("json", "CREATE TABLE t (id INT, data JSON);", "JSON"),
    ("json", "CREATE TABLE t (id INT, data JSONB);", "JSONB"),
    // Foreign keys — column-level and table-level
    (
        "fk",
        "CREATE TABLE t (id INT, cid INT REFERENCES c(id));",
        "FOREIGN KEY",
    ),
    (
        "fk",
        "CREATE TABLE t (id INT, cid INT, FOREIGN KEY (cid) REFERENCES c(id));",
        "FOREIGN KEY",
    ),
    // Temp tables
    ("temp", "CREATE TEMP TABLE t (id INT);", "TEMPORARY"),
    ("temp", "CREATE TEMPORARY TABLE t (id INT);", "TEMPORARY"),
    // Array types
    ("array", "CREATE TABLE t (id INT, tags TEXT[]);", "array"),
    ("array", "CREATE TABLE t (id INT, scores INT[]);", "array"),
    // Partition
    (
        "partition",
        "CREATE TABLE t (id INT, d DATE) PARTITION BY RANGE (d);",
        "PARTITION",
    ),
    // Truncate
    ("truncate", "TRUNCATE TABLE orders;", "TRUNCATE"),
    // Trigger
    (
        "trigger",
        "CREATE TRIGGER trg AFTER INSERT ON t FOR EACH ROW EXECUTE FUNCTION f();",
        "TRIGGER",
    ),
    // Extension
    (
        "extension",
        "CREATE EXTENSION IF NOT EXISTS pgcrypto;",
        "EXTENSION",
    ),
    // Index without ASYNC
    ("index", "CREATE INDEX idx_foo ON t(col);", "ASYNC"),
    ("index", "CREATE UNIQUE INDEX idx_bar ON t(col);", "ASYNC"),
    (
        "index-using-btree",
        "CREATE INDEX ASYNC idx ON t USING btree(col);",
        "USING",
    ),
    (
        "index-using-gin",
        "CREATE INDEX ASYNC idx ON t USING gin(col);",
        "USING",
    ),
    (
        "index-using-hash",
        "CREATE INDEX ASYNC idx ON t USING hash(col);",
        "USING",
    ),
    // USING clause after column list (parsed into index_options, not ci.using)
    (
        "index-using-btree-after-cols",
        "CREATE INDEX ASYNC idx ON t(col) USING btree;",
        "USING",
    ),
    (
        "index-using-hash-after-cols",
        "CREATE INDEX ASYNC idx ON t(col) USING hash;",
        "USING",
    ),
    // USING after column list without ASYNC (both rules should fire)
    (
        "index-using-no-async",
        "CREATE INDEX idx ON t(col) USING btree;",
        "USING",
    ),
    (
        "index-expr",
        "CREATE INDEX ASYNC idx ON t (lower(name));",
        "Expression",
    ),
    (
        "index-concurrently",
        "CREATE INDEX CONCURRENTLY idx ON t(col);",
        "CONCURRENTLY",
    ),
    (
        "index-partial",
        "CREATE INDEX ASYNC idx ON t(col) WHERE col > 0;",
        "Partial",
    ),
    // ALTER TABLE — same checks as CREATE TABLE
    (
        "alter-serial",
        "ALTER TABLE t ADD COLUMN id SERIAL;",
        "SERIAL",
    ),
    ("alter-json", "ALTER TABLE t ADD COLUMN data JSON;", "JSON"),
    (
        "alter-array",
        "ALTER TABLE t ADD COLUMN tags TEXT[];",
        "array",
    ),
    (
        "alter-fk-column",
        "ALTER TABLE t ADD COLUMN cid INT REFERENCES c(id);",
        "FOREIGN KEY",
    ),
    (
        "alter-fk-constraint",
        "ALTER TABLE t ADD CONSTRAINT fk_c FOREIGN KEY (cid) REFERENCES c(id);",
        "FOREIGN KEY",
    ),
    // INHERITS clause
    (
        "inherits",
        "CREATE TABLE child (extra INT) INHERITS (parent);",
        "INHERITS",
    ),
    // CREATE TABLE AS
    (
        "create-table-as",
        "CREATE TABLE t AS SELECT 1 AS id;",
        "CREATE TABLE AS",
    ),
    // TABLESPACE on table
    (
        "tablespace-table",
        "CREATE TABLE t (id INT) TABLESPACE my_space;",
        "TABLESPACE",
    ),
    // Temporary view
    (
        "temp-view",
        "CREATE TEMPORARY VIEW v AS SELECT 1;",
        "TEMPORARY VIEW",
    ),
    // Materialized view
    (
        "mat-view",
        "CREATE MATERIALIZED VIEW mv AS SELECT 1;",
        "MATERIALIZED VIEW",
    ),
    // CREATE DATABASE
    (
        "create-database",
        "CREATE DATABASE mydb;",
        "CREATE DATABASE",
    ),
    // CREATE POLICY
    (
        "create-policy",
        "CREATE POLICY p ON t USING (true);",
        "CREATE POLICY",
    ),
    // SAVEPOINT
    ("savepoint", "SAVEPOINT sp1;", "SAVEPOINT"),
    // RELEASE SAVEPOINT
    (
        "release-savepoint",
        "RELEASE SAVEPOINT sp1;",
        "RELEASE SAVEPOINT",
    ),
    // ROLLBACK TO SAVEPOINT
    (
        "rollback-to-savepoint",
        "ROLLBACK TO SAVEPOINT sp1;",
        "ROLLBACK TO SAVEPOINT",
    ),
    // DECLARE CURSOR
    ("declare-cursor", "DECLARE c CURSOR FOR SELECT 1;", "CURSOR"),
    // CREATE TYPE
    (
        "create-type",
        "CREATE TYPE mood AS ENUM ('happy', 'sad');",
        "CREATE TYPE",
    ),
    // CREATE SERVER
    (
        "create-server",
        "CREATE SERVER s FOREIGN DATA WRAPPER w;",
        "CREATE SERVER",
    ),
    (
        "lock-table",
        "LOCK TABLE t IN ACCESS EXCLUSIVE MODE;",
        "LOCK TABLE",
    ),
    // VACUUM
    ("vacuum", "VACUUM;", "VACUUM"),
    ("vacuum-full", "VACUUM FULL t;", "VACUUM"),
    // ALTER INDEX
    (
        "alter-index",
        "ALTER INDEX idx_name RENAME TO idx_new;",
        "ALTER INDEX",
    ),
    // Identity column with non-BIGINT type
    (
        "identity-non-bigint",
        "CREATE TABLE t (id INTEGER GENERATED ALWAYS AS IDENTITY);",
        "Identity column",
    ),
    (
        "identity-non-bigint-small",
        "CREATE TABLE t (id SMALLINT GENERATED BY DEFAULT AS IDENTITY);",
        "Identity column",
    ),
    // CREATE SEQUENCE — non-BIGINT type
    (
        "seq-non-bigint",
        "CREATE SEQUENCE s AS INTEGER;",
        "CREATE SEQUENCE with type",
    ),
    // CREATE SEQUENCE — invalid CACHE value
    (
        "seq-bad-cache",
        "CREATE SEQUENCE s CACHE 100;",
        "CACHE value",
    ),
    (
        "seq-bad-cache-2",
        "CREATE SEQUENCE s CACHE 2;",
        "CACHE value",
    ),
    // CACHE boundary: 65535 should be an error (must be 1 or >= 65536)
    (
        "seq-bad-cache-boundary",
        "CREATE SEQUENCE s CACHE 65535;",
        "CACHE value",
    ),
    // CACHE negative value
    (
        "seq-bad-cache-negative",
        "CREATE SEQUENCE s CACHE -1;",
        "CACHE value",
    ),
    // CACHE 0
    (
        "seq-bad-cache-zero",
        "CREATE SEQUENCE s CACHE 0;",
        "CACHE value",
    ),
    // Identity column with invalid CACHE value (type is fine, CACHE is not)
    (
        "identity-bad-cache",
        "CREATE TABLE t (id BIGINT GENERATED ALWAYS AS IDENTITY (CACHE 100));",
        "CACHE value",
    ),
    // ALTER TABLE — Row-Level Security
    (
        "alter-enable-rls",
        "ALTER TABLE t ENABLE ROW LEVEL SECURITY;",
        "ROW LEVEL SECURITY",
    ),
    (
        "alter-disable-rls",
        "ALTER TABLE t DISABLE ROW LEVEL SECURITY;",
        "ROW LEVEL SECURITY",
    ),
    (
        "alter-force-rls",
        "ALTER TABLE t FORCE ROW LEVEL SECURITY;",
        "ROW LEVEL SECURITY",
    ),
    (
        "alter-noforce-rls",
        "ALTER TABLE t NO FORCE ROW LEVEL SECURITY;",
        "ROW LEVEL SECURITY",
    ),
    // ALTER TABLE — Triggers
    (
        "alter-enable-trigger",
        "ALTER TABLE t ENABLE TRIGGER trg1;",
        "ENABLE TRIGGER",
    ),
    (
        "alter-disable-trigger",
        "ALTER TABLE t DISABLE TRIGGER trg1;",
        "DISABLE TRIGGER",
    ),
    (
        "alter-enable-always-trigger",
        "ALTER TABLE t ENABLE ALWAYS TRIGGER trg1;",
        "ENABLE ALWAYS TRIGGER",
    ),
    (
        "alter-enable-replica-trigger",
        "ALTER TABLE t ENABLE REPLICA TRIGGER trg1;",
        "ENABLE REPLICA TRIGGER",
    ),
    // ALTER TABLE — Replica Identity
    (
        "alter-replica-identity",
        "ALTER TABLE t REPLICA IDENTITY FULL;",
        "REPLICA IDENTITY",
    ),
    // ALTER TABLE — VALIDATE CONSTRAINT
    (
        "alter-validate-constraint",
        "ALTER TABLE t VALIDATE CONSTRAINT c1;",
        "VALIDATE CONSTRAINT",
    ),
    // Mixed expression index (simple + expression column)
    (
        "index-mixed-expr",
        "CREATE INDEX ASYNC idx ON t (col, lower(name));",
        "Expression",
    ),
    // CompoundIdentifier in index (composite-type field access = expression)
    (
        "index-compound-id",
        "CREATE INDEX ASYNC idx ON t(a.b);",
        "Expression",
    ),
    // ENABLE/DISABLE RULE
    (
        "enable-rule",
        "ALTER TABLE t ENABLE RULE r1;",
        "ENABLE RULE",
    ),
    (
        "disable-rule",
        "ALTER TABLE t DISABLE RULE r1;",
        "DISABLE RULE",
    ),
    (
        "enable-always-rule",
        "ALTER TABLE t ENABLE ALWAYS RULE r1;",
        "ENABLE ALWAYS RULE",
    ),
    (
        "enable-replica-rule",
        "ALTER TABLE t ENABLE REPLICA RULE r1;",
        "ENABLE REPLICA RULE",
    ),
    (
        "pk-using-index",
        "ALTER TABLE t ADD PRIMARY KEY USING INDEX my_idx;",
        "PRIMARY KEY USING INDEX",
    ),
    (
        "unique-using-index",
        "ALTER TABLE t ADD UNIQUE USING INDEX my_idx;",
        "UNIQUE USING INDEX",
    ),
    // COPY
    // COPY with server-side file path
    (
        "copy-from-file",
        "COPY t FROM '/tmp/data.csv';",
        "STDIN/STDOUT",
    ),
    ("copy-to-file", "COPY t TO '/tmp/data.csv';", "STDIN/STDOUT"),
    // COPY with PROGRAM
    (
        "copy-from-program",
        "COPY t FROM PROGRAM 'cat /tmp/data.csv';",
        "STDIN/STDOUT",
    ),
];

#[test]
fn error_detection_matrix() {
    for (category, sql, expected) in ERROR_CASES {
        let diags = lint_sql(sql);
        assert!(
            diags.iter().any(|d| d.message.contains(expected)),
            "[{category}] Expected error containing {expected:?} for:\n  {sql}\n  got: {diags:?}"
        );
    }
}

// ═══════════════════════════════════════════════════════════════════════
// 3. SUGGESTION VALIDITY MATRIX
// ═══════════════════════════════════════════════════════════════════════
// Every SQL replacement the linter suggests must itself be valid DSQL.
// Column-type suggestions are wrapped in CREATE TABLE; command suggestions
// are tested directly.

const SUGGESTED_COLUMN_TYPES: &[&str] = &["BIGINT GENERATED BY DEFAULT AS IDENTITY (CACHE 1)"];

#[test]
fn suggested_column_types_are_dsql_valid() {
    for col_type in SUGGESTED_COLUMN_TYPES {
        let sql = format!("CREATE TABLE _test (id {col_type} PRIMARY KEY);");
        let diags = lint_sql(&sql);
        assert!(
            diags.is_empty(),
            "Suggested type `{col_type}` triggers errors:\n  SQL: {sql}\n  Errors: {diags:?}"
        );
    }
}

#[test]
fn suggested_async_index_is_valid() {
    let sql = "CREATE INDEX ASYNC idx_test ON t(col);";
    let diags = lint_sql(sql);
    assert!(
        !diags.iter().any(|d| d.message.contains("not supported")),
        "CREATE INDEX ASYNC should be valid, got: {diags:?}"
    );
}

// ═══════════════════════════════════════════════════════════════════════
// 4. FALSE-POSITIVE MATRIX
// ═══════════════════════════════════════════════════════════════════════
// Valid SQL that must NOT trigger any errors. Each row targets a specific
// rule to verify it doesn't over-match.

const FALSE_POSITIVE_CASES: &[(&str, &str)] = &[
    // Column named 'serial_number' should not trigger SERIAL rule
    (
        "CREATE TABLE t (id UUID PRIMARY KEY, serial_number VARCHAR(100));",
        "SERIAL",
    ),
    // TEXT column named 'json_data' should not trigger JSON rule
    (
        "CREATE TABLE t (id UUID PRIMARY KEY, json_data TEXT);",
        "JSON",
    ),
    // Table named 'temporary_cache' should not trigger TEMP rule
    (
        "CREATE TABLE temporary_cache (id UUID PRIMARY KEY);",
        "TEMPORARY",
    ),
    // TEXT column named 'tags' should not trigger array rule
    ("CREATE TABLE t (id UUID PRIMARY KEY, tags TEXT);", "array"),
    // Column named 'partition_key' should not trigger PARTITION rule
    (
        "CREATE TABLE t (id UUID PRIMARY KEY, partition_key VARCHAR(50));",
        "PARTITION",
    ),
    // DELETE FROM should not trigger TRUNCATE rule
    ("DELETE FROM events WHERE id = 1;", "TRUNCATE"),
    // GENERATED ALWAYS AS IDENTITY should not trigger SERIAL
    (
        "CREATE TABLE t (id BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY);",
        "SERIAL",
    ),
    // GENERATED BY DEFAULT AS IDENTITY should not trigger SERIAL
    (
        "CREATE TABLE t (id BIGINT GENERATED BY DEFAULT AS IDENTITY PRIMARY KEY);",
        "SERIAL",
    ),
    // Column named 'inherits_from' should not trigger INHERITS rule
    (
        "CREATE TABLE t (id UUID PRIMARY KEY, inherits_from TEXT);",
        "INHERITS",
    ),
    // Valid index should not trigger USING, Expression, or Partial rules
    ("CREATE INDEX ASYNC idx ON t(col);", "USING"),
    ("CREATE INDEX ASYNC idx ON t(col);", "Expression"),
    ("CREATE INDEX ASYNC idx ON t(col);", "Partial"),
    // Regular CREATE VIEW should not trigger temporary or materialized view rules
    ("CREATE VIEW v AS SELECT 1;", "TEMPORARY VIEW"),
    ("CREATE VIEW v AS SELECT 1;", "MATERIALIZED VIEW"),
    // BIGINT identity columns should not trigger identity error
    (
        "CREATE TABLE t (id BIGINT GENERATED ALWAYS AS IDENTITY (CACHE 1));",
        "Identity column",
    ),
    (
        "CREATE TABLE t (id BIGINT GENERATED BY DEFAULT AS IDENTITY (CACHE 1));",
        "Identity column",
    ),
    // Valid CREATE SEQUENCE should not trigger errors
    ("CREATE SEQUENCE s AS BIGINT CACHE 65536;", "not supported"),
    ("CREATE SEQUENCE s CACHE 1;", "invalid"),
    ("CREATE SEQUENCE s CACHE 65536;", "invalid"),
    // Schema-qualified column in index is not an expression index
    ("CREATE INDEX ASYNC idx ON t(col);", "Expression"),
    // CONCURRENTLY should only produce one error, not also an ASYNC error
    // (tested via the error matrix — the CONCURRENTLY test checks for CONCURRENTLY,
    //  this ensures it doesn't ALSO say "ASYNC")
    ("CREATE INDEX CONCURRENTLY idx ON t(col);", "without ASYNC"),
    // Identity column with valid CACHE should not trigger CACHE error
    (
        "CREATE TABLE t (id BIGINT GENERATED ALWAYS AS IDENTITY (CACHE 65536));",
        "CACHE value",
    ),
    // CACHE 65537 is also valid
    ("CREATE SEQUENCE s CACHE 65537;", "CACHE value"),
    // Plain ROLLBACK should not trigger ROLLBACK TO SAVEPOINT rule
    ("ROLLBACK;", "SAVEPOINT"),
    // Plain ALTER TABLE ADD COLUMN should not trigger RLS/trigger rules
    ("ALTER TABLE t ADD COLUMN x TEXT;", "ROW LEVEL SECURITY"),
    ("ALTER TABLE t ADD COLUMN x TEXT;", "TRIGGER"),
    ("ALTER TABLE t ADD COLUMN x TEXT;", "REPLICA IDENTITY"),
    // RENAME/OWNER should not trigger VALIDATE CONSTRAINT
    ("ALTER TABLE t OWNER TO new_owner;", "VALIDATE CONSTRAINT"),
    // Plain ALTER TABLE ADD COLUMN should not trigger RULE diagnostics
    ("ALTER TABLE t ADD COLUMN x TEXT;", "RULE"),
    // COPY FROM STDIN / COPY TO STDOUT are valid in DSQL
    ("COPY t FROM STDIN;", "COPY"),
    ("COPY t TO STDOUT;", "COPY"),
    // ALTER TABLE ADD CHECK (not USING INDEX) should not trigger USING INDEX
    (
        "ALTER TABLE t ADD CONSTRAINT c CHECK (id > 0);",
        "USING INDEX",
    ),
];

/// Valid SQL that must produce zero errors, period. No substring matching —
/// these statements are entirely clean and should never trigger anything.
const CLEAN_STATEMENTS: &[&str] = &[
    "CREATE VIEW v AS SELECT 1;",
    "ALTER TABLE t ADD COLUMN description TEXT;",
    "INSERT INTO t (data) VALUES ('TRUNCATE TABLE foo; CREATE TRIGGER bar');",
    "SELECT * FROM t WHERE id = 1;",
    "UPDATE t SET name = 'foo' WHERE id = 1;",
    "DELETE FROM t WHERE id = 1;",
    "CREATE VIEW v2 AS SELECT 1;",
    "BEGIN;",
    "BEGIN ISOLATION LEVEL REPEATABLE READ;",
];

#[test]
fn false_positive_matrix() {
    for (sql, unexpected) in FALSE_POSITIVE_CASES {
        let diags = lint_sql(sql);
        assert!(
            !diags.iter().any(|d| d.message.contains(unexpected)),
            "False positive! `{unexpected}` found in errors for:\n  {sql}\n  Errors: {diags:?}"
        );
    }
}

#[test]
fn clean_statements_produce_zero_errors() {
    for sql in CLEAN_STATEMENTS {
        let diags = lint_sql(sql);
        assert!(
            diags.is_empty(),
            "Clean SQL triggered errors:\n  SQL: {sql}\n  Errors: {diags:?}"
        );
    }
}

// ═══════════════════════════════════════════════════════════════════════
// 5. ADDITIONAL ERROR DETECTION
// ═══════════════════════════════════════════════════════════════════════

const ADDITIONAL_ERROR_CASES: &[(&str, &str)] = &[
    (
        "ALTER TABLE t ADD COLUMN status VARCHAR(50) DEFAULT 'pending';",
        "ADD COLUMN",
    ),
    (
        "ALTER TABLE t ADD COLUMN status VARCHAR(50) NOT NULL;",
        "ADD COLUMN",
    ),
    // Transaction isolation level
    ("BEGIN ISOLATION LEVEL SERIALIZABLE;", "isolation level"),
    ("BEGIN ISOLATION LEVEL READ COMMITTED;", "isolation level"),
    (
        "SET TRANSACTION ISOLATION LEVEL SERIALIZABLE;",
        "SET TRANSACTION",
    ),
    // CREATE SEQUENCE without CACHE
    ("CREATE SEQUENCE s;", "CACHE"),
    ("CREATE SEQUENCE s INCREMENT 1;", "CACHE"),
    // ALTER TABLE operations
    ("ALTER TABLE t DROP COLUMN name;", "DROP COLUMN"),
    ("ALTER TABLE t ALTER COLUMN name TYPE TEXT;", "ALTER COLUMN"),
    (
        "ALTER TABLE t ALTER COLUMN name SET NOT NULL;",
        "SET NOT NULL",
    ),
    (
        "ALTER TABLE t ALTER COLUMN name DROP NOT NULL;",
        "DROP NOT NULL",
    ),
    (
        "ALTER TABLE t ALTER COLUMN name SET DEFAULT 'foo';",
        "SET DEFAULT",
    ),
    (
        "ALTER TABLE t ALTER COLUMN name DROP DEFAULT;",
        "DROP DEFAULT",
    ),
    ("ALTER TABLE t ADD CONSTRAINT c CHECK (id > 0);", "CHECK"),
    ("ALTER TABLE t ADD CONSTRAINT c UNIQUE (name);", "UNIQUE"),
    ("ALTER TABLE t DROP CONSTRAINT c;", "DROP CONSTRAINT"),
    // Identity column without CACHE — CREATE TABLE
    (
        "CREATE TABLE t (id BIGINT GENERATED ALWAYS AS IDENTITY);",
        "CACHE",
    ),
    (
        "CREATE TABLE t (id BIGINT GENERATED BY DEFAULT AS IDENTITY);",
        "CACHE",
    ),
    // Identity column in ALTER TABLE ADD COLUMN — not supported in DSQL
    (
        "ALTER TABLE t ADD COLUMN id BIGINT GENERATED ALWAYS AS IDENTITY;",
        "not supported in ALTER TABLE ADD COLUMN",
    ),
    (
        "ALTER TABLE t ADD COLUMN id BIGINT GENERATED BY DEFAULT AS IDENTITY;",
        "not supported in ALTER TABLE ADD COLUMN",
    ),
    // ALTER COLUMN ADD GENERATED AS IDENTITY
    (
        "ALTER TABLE t ALTER COLUMN id ADD GENERATED ALWAYS AS IDENTITY;",
        "ADD GENERATED AS IDENTITY",
    ),
    (
        "ALTER TABLE t ALTER COLUMN id ADD GENERATED BY DEFAULT AS IDENTITY;",
        "ADD GENERATED AS IDENTITY",
    ),
    (
        "ALTER TABLE t ALTER COLUMN id ADD GENERATED ALWAYS AS IDENTITY (CACHE 1);",
        "ADD GENERATED AS IDENTITY",
    ),
];

const ADDITIONAL_FALSE_POSITIVES: &[(&str, &str)] = &[
    ("ALTER TABLE t ADD COLUMN status VARCHAR(50);", "ADD COLUMN"),
    // REPEATABLE READ should not error
    ("BEGIN ISOLATION LEVEL REPEATABLE READ;", "isolation"),
    // Plain BEGIN should not error about isolation
    ("BEGIN;", "isolation"),
    // SEQUENCE with CACHE should not error
    ("CREATE SEQUENCE s CACHE 1;", "CACHE clause"),
    ("CREATE SEQUENCE s CACHE 65536;", "CACHE clause"),
    // Supported ALTER TABLE operations should not error
    ("ALTER TABLE t ADD COLUMN name TEXT;", "DROP COLUMN"),
    ("ALTER TABLE t OWNER TO new_owner;", "DROP"),
    ("ALTER TABLE t RENAME COLUMN a TO b;", "ALTER COLUMN"),
    // Identity column WITH CACHE should not error about missing CACHE
    (
        "CREATE TABLE t (id BIGINT GENERATED ALWAYS AS IDENTITY (CACHE 1));",
        "CACHE clause",
    ),
    (
        "CREATE TABLE t (id BIGINT GENERATED ALWAYS AS IDENTITY (CACHE 65536));",
        "CACHE clause",
    ),
    // ALTER TABLE ADD COLUMN identity errors about identity, not CACHE
    (
        "ALTER TABLE t ADD COLUMN id BIGINT GENERATED ALWAYS AS IDENTITY (CACHE 1);",
        "CACHE clause",
    ),
];

#[test]
fn additional_error_detection_matrix() {
    for (sql, expected) in ADDITIONAL_ERROR_CASES {
        let diags = lint_sql(sql);
        assert!(
            diags.iter().any(|d| d.message.contains(expected)),
            "Expected error containing {expected:?} for:\n  {sql}\n  got: {diags:?}"
        );
    }
}

#[test]
fn additional_false_positive_matrix() {
    for (sql, unexpected) in ADDITIONAL_FALSE_POSITIVES {
        let diags = lint_sql(sql);
        assert!(
            !diags.iter().any(|d| d.message.contains(unexpected)),
            "Unexpected error containing {unexpected:?} for:\n  {sql}\n  got: {diags:?}"
        );
    }
}

// ═══════════════════════════════════════════════════════════════════════
// 6. FIXTURE-BASED TESTS
// ═══════════════════════════════════════════════════════════════════════

/// Every supported DSQL type, constraint, and valid DDL pattern in one file.
/// Must produce zero errors.
#[test]
fn fixture_clean_all_types() {
    let sql = include_str!("fixtures/clean_all_types.sql");
    let diags = lint_sql(sql);
    assert!(
        diags.is_empty(),
        "clean_all_types.sql should have zero errors, got:\n{diags:#?}"
    );
}

/// Realistic migration with known incompatibilities — verify each is caught.
#[test]
fn fixture_sample_migration() {
    let sql = include_str!("fixtures/sample_migration.sql");
    let diags = lint_sql(sql);

    let expected = &[
        "SERIAL",
        "FOREIGN KEY",
        "JSON",
        "TRUNCATE",
        "TEMPORARY",
        "array",
        "ASYNC",
        "INHERITS",
        "TEMPORARY VIEW",
        "SAVEPOINT",
        "ROLLBACK TO SAVEPOINT",
        "CREATE DATABASE",
    ];
    for msg in expected {
        assert!(
            diags.iter().any(|d| d.message.contains(msg)),
            "sample_migration.sql: missing expected error containing {msg:?}\n  got: {diags:#?}"
        );
    }

    // Valid DML in the fixture should not produce errors
    assert!(
        !diags.iter().any(|d| d.message.contains("INSERT")
            || d.message.contains("SELECT")
            || d.message.contains("DELETE")),
        "DML should not produce errors: {diags:#?}"
    );
}
