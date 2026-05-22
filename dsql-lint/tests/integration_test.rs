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

use dsql_lint::{lint_sql, LintRule};
use strum::IntoEnumIterator;

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
    // JSONB
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
    // ALTER TABLE
    (
        "alter-serial",
        "ALTER TABLE t ADD COLUMN id SERIAL;",
        "SERIAL",
    ),
    (
        "alter-json",
        "ALTER TABLE t ADD COLUMN data JSONB;",
        "JSONB",
    ),
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
    // ALTER FUNCTION — property change (Actions). Other variants supported on DSQL.
    (
        "alter-function-immutable",
        "ALTER FUNCTION fn() IMMUTABLE;",
        "ALTER FUNCTION",
    ),
    (
        "alter-function-strict",
        "ALTER FUNCTION fn() STRICT;",
        "ALTER FUNCTION",
    ),
    // ALTER AGGREGATE — entire family rejected
    (
        "alter-aggregate-rename",
        "ALTER AGGREGATE my_agg(integer) RENAME TO new_agg;",
        "ALTER AGGREGATE",
    ),
    (
        "alter-aggregate-owner",
        "ALTER AGGREGATE my_agg(*) OWNER TO admin;",
        "ALTER AGGREGATE",
    ),
    // ALTER POLICY
    (
        "alter-policy",
        "ALTER POLICY p ON t USING (true);",
        "ALTER POLICY",
    ),
    // ALTER TYPE
    (
        "alter-type-add-value",
        "ALTER TYPE mood ADD VALUE 'neutral';",
        "ALTER TYPE",
    ),
    (
        "alter-type-rename",
        "ALTER TYPE mood RENAME TO feeling;",
        "ALTER TYPE",
    ),
    // ALTER ROLE — WithOptions and Set are rejected
    (
        "alter-role-password",
        "ALTER ROLE r WITH PASSWORD 'pw';",
        "PASSWORD",
    ),
    (
        "alter-role-valid-until",
        "ALTER ROLE r VALID UNTIL 'infinity';",
        "VALID UNTIL",
    ),
    (
        "alter-role-superuser",
        "ALTER ROLE r SUPERUSER;",
        "SUPERUSER",
    ),
    (
        "alter-role-createrole",
        "ALTER ROLE r CREATEROLE;",
        "CREATEROLE",
    ),
    (
        "alter-role-set",
        "ALTER ROLE r SET work_mem = '64MB';",
        "ALTER ROLE",
    ),
    // ALTER USER — every variant rejected
    (
        "alter-user-password",
        "ALTER USER u WITH PASSWORD 'pw';",
        "ALTER USER",
    ),
    // DROP MATERIALIZED VIEW / TYPE / TRIGGER / POLICY
    (
        "drop-materialized-view",
        "DROP MATERIALIZED VIEW mv;",
        "MATERIALIZED VIEW",
    ),
    ("drop-type", "DROP TYPE mood;", "DROP TYPE"),
    ("drop-trigger", "DROP TRIGGER trg ON t;", "DROP TRIGGER"),
    ("drop-policy", "DROP POLICY p ON t;", "DROP POLICY"),
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

// FALSE_POSITIVE_CASES, ADDITIONAL_ERROR_CASES, and ADDITIONAL_FALSE_POSITIVES
// live in tests/common/mod.rs so the grammar-diff in-tree corpus mirror
// (tests/grammar_corpus/in_tree/) can share them with these tests.

#[test]
fn false_positive_matrix() {
    for (sql, unexpected) in common::FALSE_POSITIVE_CASES {
        let diags = lint_sql(sql);
        assert!(
            !diags.iter().any(|d| d.message.contains(unexpected)),
            "False positive! `{unexpected}` found in errors for:\n  {sql}\n  Errors: {diags:?}"
        );
    }
}

#[test]
fn clean_statements_produce_zero_errors() {
    for (label, sql, _, _) in common::CLEAN_STATEMENTS {
        let diags = lint_sql(sql);
        assert!(
            diags.is_empty(),
            "[{label}] Clean SQL triggered errors:\n  SQL: {sql}\n  Errors: {diags:?}"
        );
    }
}

// ═══════════════════════════════════════════════════════════════════════
// 5. ADDITIONAL ERROR DETECTION
// ═══════════════════════════════════════════════════════════════════════

#[test]
fn additional_error_detection_matrix() {
    for (sql, expected) in common::ADDITIONAL_ERROR_CASES {
        let diags = lint_sql(sql);
        assert!(
            diags.iter().any(|d| d.message.contains(expected)),
            "Expected error containing {expected:?} for:\n  {sql}\n  got: {diags:?}"
        );
    }
}

#[test]
fn additional_false_positive_matrix() {
    for (sql, unexpected) in common::ADDITIONAL_FALSE_POSITIVES {
        let diags = lint_sql(sql);
        assert!(
            !diags.iter().any(|d| d.message.contains(unexpected)),
            "Unexpected error containing {unexpected:?} for:\n  {sql}\n  got: {diags:?}"
        );
    }
}

// ═══════════════════════════════════════════════════════════════════════
// 6. DDL TRANSACTION DETECTION
// ═══════════════════════════════════════════════════════════════════════

#[test]
fn multi_ddl_in_transaction_detected() {
    let sql = "BEGIN;\nCREATE TABLE a (id INT);\nCREATE TABLE b (id INT);\nCOMMIT;";
    let diags = lint_sql(sql);
    assert!(
        diags.iter().any(|d| d.message.contains("2 DDL statements")),
        "Should detect 2 DDL in one transaction: {diags:?}"
    );
}

#[test]
fn three_ddl_in_transaction_detected() {
    let sql = "BEGIN;\nCREATE TABLE a (id INT);\nCREATE TABLE b (id INT);\nCREATE INDEX ASYNC idx ON a(id);\nCOMMIT;";
    let diags = lint_sql(sql);
    assert!(
        diags.iter().any(|d| d.message.contains("3 DDL statements")),
        "Should detect 3 DDL in one transaction: {diags:?}"
    );
}

#[test]
fn clean_multi_statement_cases_produce_no_ddl_transaction_errors() {
    for (label, sql, _cleanup) in common::CLEAN_MULTI_STATEMENT_CASES {
        let diags = lint_sql(sql);
        assert!(
            !diags.iter().any(|d| d.message.contains("DDL statements")),
            "[{label}] Clean multi-statement case triggered DDL transaction error:\n  SQL: {sql}\n  Errors: {diags:?}"
        );
    }
}

#[test]
fn multiple_transactions_each_checked() {
    let sql = "BEGIN;\nCREATE TABLE a (id INT);\nCREATE TABLE b (id INT);\nCOMMIT;\nBEGIN;\nCREATE TABLE c (id INT);\nCOMMIT;";
    let diags = lint_sql(sql);
    let ddl_txn_diags: Vec<_> = diags
        .iter()
        .filter(|d| d.message.contains("DDL statements"))
        .collect();
    assert_eq!(
        ddl_txn_diags.len(),
        1,
        "Only the first transaction should be flagged: {ddl_txn_diags:?}"
    );
}

#[test]
fn alter_and_drop_count_as_ddl() {
    let sql = "BEGIN;\nALTER TABLE t ADD COLUMN x TEXT;\nDROP TABLE IF EXISTS old_t;\nCOMMIT;";
    let diags = lint_sql(sql);
    assert!(
        diags.iter().any(|d| d.message.contains("2 DDL statements")),
        "ALTER TABLE and DROP TABLE should both count as DDL: {diags:?}"
    );
}

#[test]
fn rollback_terminates_transaction_no_bleed() {
    let sql = "BEGIN;\nCREATE TABLE a (id INT);\nCREATE TABLE b (id INT);\nROLLBACK;\nBEGIN;\nCREATE TABLE c (id INT);\nCOMMIT;";
    let diags = lint_sql(sql);
    let ddl_txn_diags: Vec<_> = diags
        .iter()
        .filter(|d| d.message.contains("DDL statements"))
        .collect();
    assert!(
        ddl_txn_diags.is_empty(),
        "Rolled-back transaction should not be flagged, and DDL count must not bleed: {ddl_txn_diags:?}"
    );
}

#[test]
fn begin_without_commit_no_diagnostic() {
    let sql = "BEGIN;\nCREATE TABLE a (id INT);\nCREATE TABLE b (id INT);";
    let diags = lint_sql(sql);
    assert!(
        !diags.iter().any(|d| d.message.contains("DDL statements")),
        "Unclosed transaction should not be flagged: {diags:?}"
    );
}

#[test]
fn truncate_counts_as_ddl() {
    let sql = "BEGIN;\nCREATE TABLE a (id INT);\nTRUNCATE TABLE a;\nCOMMIT;";
    let diags = lint_sql(sql);
    assert!(
        diags.iter().any(|d| d.message.contains("2 DDL statements")),
        "TRUNCATE should count as DDL: {diags:?}"
    );
}

#[test]
fn nested_begin_does_not_reset_ddl_count() {
    // PostgreSQL/DSQL treats BEGIN inside an open transaction as a no-op warning.
    // Both DDLs are in the same transaction.
    let sql = "BEGIN;\nCREATE TABLE a (id INT);\nBEGIN;\nCREATE TABLE b (id INT);\nCOMMIT;";
    let diags = lint_sql(sql);
    assert!(
        diags.iter().any(|d| d.message.contains("2 DDL statements")),
        "Nested BEGIN is a no-op — both DDLs are in the same transaction: {diags:?}"
    );
}

#[test]
fn parse_error_inside_transaction_warns() {
    let sql = "BEGIN;\nCREATE TABLE a (id INT);\nNOT VALID SQL ???;\nCOMMIT;";
    let diags = lint_sql(sql);
    assert!(
        diags.iter().any(|d| d
            .message
            .contains("Cannot parse statement inside transaction")),
        "Should warn about unparseable statement inside transaction: {diags:?}"
    );
}

// ═══════════════════════════════════════════════════════════════════════
// 7. FIXTURE-BASED TESTS
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
        "JSONB",
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

// ═══════════════════════════════════════════════════════════════════════
// 8. LINT RULE COVERAGE ENFORCEMENT
// ═══════════════════════════════════════════════════════════════════════
// The exhaustive match in `cluster_test_for_rule` is the primary
// enforcement: new LintRule variant without a match arm = compile error.
// This test validates that every non-None mapping actually produces
// the expected diagnostic, catching stale or wrong SQL/message pairs.

#[test]
fn lint_rule_mapping_produces_expected_diagnostics() {
    for rule in LintRule::iter() {
        if let Some((sql, expected_msg)) = common::cluster_test_for_rule(rule) {
            let diags = lint_sql(sql);
            assert!(
                diags.iter().any(|d| d.rule == rule && d.message.contains(expected_msg)),
                "Rule {rule:?} mapping SQL doesn't trigger expected diagnostic.\n  SQL: {sql}\n  Expected: {expected_msg}\n  Got: {diags:?}"
            );
        }
    }
}
