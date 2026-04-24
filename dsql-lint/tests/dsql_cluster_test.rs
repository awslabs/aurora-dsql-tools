//! DSQL cluster integration tests.
//!
//! These tests validate that:
//!   1. Every fixable SQL pattern, after `fix_sql`, executes successfully on a real DSQL cluster
//!   2. Every SQL pattern we pass through as "clean" actually executes on DSQL
//!
//! All tests are `#[ignore]`-gated so `cargo test` skips them locally.
//! CI runs them via `cargo test --ignored` with `DSQL_ENDPOINT` set.
//!
//! To run locally:
//!   DSQL_ENDPOINT=<host> cargo test --ignored -- --test-threads=1
//!
//! Prerequisites: `aws` CLI and `psql` in PATH, valid AWS credentials.

mod common;

use std::process::Command;
use std::thread;
use std::time::Duration;

use dsql_lint::{fix_sql, LintRule};
use strum::IntoEnumIterator;

const MAX_RETRIES: usize = 5;
const RETRY_BASE_MS: u64 = 2000;

fn endpoint() -> String {
    std::env::var("DSQL_ENDPOINT").expect("DSQL_ENDPOINT must be set to a DSQL cluster hostname")
}

fn region() -> String {
    std::env::var("DSQL_REGION").unwrap_or_else(|_| "us-east-1".to_string())
}

fn generate_token(endpoint: &str, region: &str) -> String {
    let output = Command::new("aws")
        .args([
            "dsql",
            "generate-db-connect-admin-auth-token",
            "--hostname",
            endpoint,
            "--region",
            region,
        ])
        .output()
        .expect("failed to run `aws` CLI");
    assert!(
        output.status.success(),
        "Token generation failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_string()
}

fn psql_cmd(endpoint: &str, token: &str) -> Command {
    let mut cmd = Command::new("psql");
    cmd.env("PGHOST", endpoint)
        .env("PGPORT", "5432")
        .env("PGUSER", "admin")
        .env("PGPASSWORD", token)
        .env("PGDATABASE", "postgres")
        .env("PGSSLMODE", "require");
    cmd
}

fn run_sql_once(endpoint: &str, token: &str, sql: &str) -> Result<String, String> {
    let output = psql_cmd(endpoint, token)
        .args(["-c", sql])
        .output()
        .expect("failed to run `psql`");
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    } else {
        Err(String::from_utf8_lossy(&output.stderr).to_string())
    }
}

/// Run SQL with retries for DSQL OC001 (schema updated by another transaction).
fn run_sql(endpoint: &str, token: &str, sql: &str) -> Result<String, String> {
    for attempt in 0..MAX_RETRIES {
        match run_sql_once(endpoint, token, sql) {
            Ok(out) => return Ok(out),
            Err(err) if err.contains("OC001") && attempt < MAX_RETRIES - 1 => {
                thread::sleep(Duration::from_millis(RETRY_BASE_MS * (attempt as u64 + 1)));
            }
            Err(err) => return Err(err),
        }
    }
    unreachable!()
}

fn run_sql_file(endpoint: &str, token: &str, sql: &str) -> Result<String, String> {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("test.sql");
    std::fs::write(&path, sql).unwrap();
    for attempt in 0..MAX_RETRIES {
        let output = psql_cmd(endpoint, token)
            .args(["-f", path.to_str().unwrap()])
            .output()
            .expect("failed to run `psql`");
        if output.status.success() {
            return Ok(String::from_utf8_lossy(&output.stdout).to_string());
        }
        let err = String::from_utf8_lossy(&output.stderr).to_string();
        if err.contains("OC001") && attempt < MAX_RETRIES - 1 {
            thread::sleep(Duration::from_millis(RETRY_BASE_MS * (attempt as u64 + 1)));
            continue;
        }
        return Err(err);
    }
    unreachable!()
}

fn cleanup(endpoint: &str, token: &str, sql: &str) {
    for attempt in 0..MAX_RETRIES {
        match run_sql_once(endpoint, token, sql) {
            Ok(_) => return,
            Err(err) if err.contains("OC001") && attempt < MAX_RETRIES - 1 => {
                thread::sleep(Duration::from_millis(RETRY_BASE_MS * (attempt as u64 + 1)));
            }
            _ => return,
        }
    }
}

fn run_cleanup_stmts(endpoint: &str, token: &str, cleanup_sql: &str) {
    for stmt in cleanup_sql.split(';') {
        let stmt = stmt.trim();
        if !stmt.is_empty() {
            cleanup(endpoint, token, &format!("{stmt};"));
        }
    }
}

fn ensure_base_table(endpoint: &str, token: &str) {
    run_sql(
        endpoint,
        token,
        "CREATE TABLE IF NOT EXISTS _clust_base (id INT, col INT);",
    )
    .expect("Failed to create base table for cluster tests");
}

// ═══════════════════════════════════════════════════════════════════════
// 1. FIX VALIDATION MATRIX
// ═══════════════════════════════════════════════════════════════════════
// Each entry: (label, unfixed_sql, cleanup_sql)
// The test runs fix_sql on the input, then executes the result on the cluster.

const FIX_MATRIX: &[(&str, &str, &str)] = &[
    // Tier 1 — Fixed
    (
        "serial",
        "CREATE TABLE _clust_serial (id SERIAL PRIMARY KEY);",
        "DROP TABLE IF EXISTS _clust_serial;",
    ),
    (
        "bigserial",
        "CREATE TABLE _clust_bigserial (id BIGSERIAL PRIMARY KEY);",
        "DROP TABLE IF EXISTS _clust_bigserial;",
    ),
    (
        "serial4",
        "CREATE TABLE _clust_serial4 (id SERIAL4 PRIMARY KEY);",
        "DROP TABLE IF EXISTS _clust_serial4;",
    ),
    (
        "serial8",
        "CREATE TABLE _clust_serial8 (id SERIAL8 PRIMARY KEY);",
        "DROP TABLE IF EXISTS _clust_serial8;",
    ),
    (
        "smallserial",
        "CREATE TABLE _clust_smallserial (id SMALLSERIAL PRIMARY KEY);",
        "DROP TABLE IF EXISTS _clust_smallserial;",
    ),
    (
        "serial2",
        "CREATE TABLE _clust_serial2 (id SERIAL2 PRIMARY KEY);",
        "DROP TABLE IF EXISTS _clust_serial2;",
    ),
    (
        "json-col",
        "CREATE TABLE _clust_json (id INT, data JSON);",
        "DROP TABLE IF EXISTS _clust_json;",
    ),
    (
        "jsonb-col",
        "CREATE TABLE _clust_jsonb (id INT, data JSONB);",
        "DROP TABLE IF EXISTS _clust_jsonb;",
    ),
    (
        "index-async",
        "CREATE INDEX _clust_idx1 ON _clust_base(col);",
        "DROP INDEX IF EXISTS _clust_idx1;",
    ),
    (
        "index-concurrently",
        "CREATE INDEX CONCURRENTLY _clust_idx2 ON _clust_base(col);",
        "DROP INDEX IF EXISTS _clust_idx2;",
    ),
    (
        "index-using-btree",
        "CREATE INDEX ASYNC _clust_idx3 ON _clust_base USING btree(col);",
        "DROP INDEX IF EXISTS _clust_idx3;",
    ),
    (
        "seq-type",
        "CREATE SEQUENCE _clust_seq1 AS INTEGER CACHE 1;",
        "DROP SEQUENCE IF EXISTS _clust_seq1;",
    ),
    (
        "seq-missing-cache",
        "CREATE SEQUENCE _clust_seq2;",
        "DROP SEQUENCE IF EXISTS _clust_seq2;",
    ),
    (
        "seq-bad-cache",
        "CREATE SEQUENCE _clust_seq3 CACHE 100;",
        "DROP SEQUENCE IF EXISTS _clust_seq3;",
    ),
    (
        "begin-serializable",
        "BEGIN ISOLATION LEVEL SERIALIZABLE;",
        "ROLLBACK;",
    ),
    (
        "identity-missing-cache",
        "CREATE TABLE _clust_ident1 (id BIGINT GENERATED ALWAYS AS IDENTITY);",
        "DROP TABLE IF EXISTS _clust_ident1;",
    ),
    (
        "identity-bad-cache",
        "CREATE TABLE _clust_ident2 (id BIGINT GENERATED ALWAYS AS IDENTITY (CACHE 100));",
        "DROP TABLE IF EXISTS _clust_ident2;",
    ),
    // Tier 2 — FixedWithWarning
    (
        "column-fk",
        "CREATE TABLE _clust_fk1 (id INT, cid INT REFERENCES _clust_base(id));",
        "DROP TABLE IF EXISTS _clust_fk1;",
    ),
    (
        "table-fk",
        "CREATE TABLE _clust_fk2 (id INT, cid INT, FOREIGN KEY (cid) REFERENCES _clust_base(id));",
        "DROP TABLE IF EXISTS _clust_fk2;",
    ),
    (
        "temp-table",
        "CREATE TEMP TABLE _clust_temp (id INT);",
        "DROP TABLE IF EXISTS _clust_temp;",
    ),
    (
        "tablespace",
        "CREATE TABLE _clust_tblsp (id INT) TABLESPACE my_space;",
        "DROP TABLE IF EXISTS _clust_tblsp;",
    ),
    (
        "partition-by",
        "CREATE TABLE _clust_part (id INT, d DATE) PARTITION BY RANGE (d);",
        "DROP TABLE IF EXISTS _clust_part;",
    ),
    (
        "inherits",
        "CREATE TABLE _clust_inherit (extra INT) INHERITS (_clust_base);",
        "DROP TABLE IF EXISTS _clust_inherit;",
    ),
    (
        "temp-view",
        "CREATE TEMP VIEW _clust_tmpview AS SELECT 1;",
        "DROP VIEW IF EXISTS _clust_tmpview;",
    ),
    (
        "identity-non-bigint",
        "CREATE TABLE _clust_identtype (id INT GENERATED ALWAYS AS IDENTITY (CACHE 1));",
        "DROP TABLE IF EXISTS _clust_identtype;",
    ),
    (
        "index-using-gin",
        "CREATE INDEX ASYNC _clust_idx_gin ON _clust_base USING gin(col);",
        "DROP INDEX IF EXISTS _clust_idx_gin;",
    ),
    (
        "index-using-hash",
        "CREATE INDEX ASYNC _clust_idx_hash ON _clust_base USING hash(col);",
        "DROP INDEX IF EXISTS _clust_idx_hash;",
    ),
    // Cross-cutting
    (
        "serial-plus-fk",
        "CREATE TABLE _clust_combo (id SERIAL PRIMARY KEY, cid INT REFERENCES _clust_base(id));",
        "DROP TABLE IF EXISTS _clust_combo;",
    ),
    (
        "alter-add-col-json",
        "ALTER TABLE _clust_base ADD COLUMN extra_data JSON;",
        "ALTER TABLE _clust_base DROP COLUMN IF EXISTS extra_data;",
    ),
    (
        "alter-add-col-fk",
        "ALTER TABLE _clust_base ADD COLUMN extra_ref INT REFERENCES _clust_base(id);",
        "ALTER TABLE _clust_base DROP COLUMN IF EXISTS extra_ref;",
    ),
    (
        "alter-fk-only",
        "ALTER TABLE _clust_base ADD CONSTRAINT _clust_fk FOREIGN KEY (col) REFERENCES _clust_base(id);",
        "",
    ),
    (
        "alter-mixed-fk-col",
        "ALTER TABLE _clust_base ADD CONSTRAINT _clust_fk2 FOREIGN KEY (col) REFERENCES _clust_base(id), ADD COLUMN mix_col INT;",
        "ALTER TABLE _clust_base DROP COLUMN IF EXISTS mix_col;",
    ),
    (
        "begin-read-committed",
        "BEGIN ISOLATION LEVEL READ COMMITTED;",
        "ROLLBACK;",
    ),
    // Unfixable — identity in ALTER TABLE ADD COLUMN
    (
        "alter-add-col-identity",
        "ALTER TABLE _clust_base ADD COLUMN ident_col BIGINT GENERATED ALWAYS AS IDENTITY;",
        "ALTER TABLE _clust_base DROP COLUMN IF EXISTS ident_col;",
    ),
];

#[test]
#[ignore = "requires DSQL cluster — run via `cargo test --ignored` with DSQL_ENDPOINT set"]
fn fix_matrix_against_cluster() {
    let ep = endpoint();
    let region = region();
    let token = generate_token(&ep, &region);
    cleanup(&ep, &token, "DROP TABLE IF EXISTS _clust_base CASCADE;");
    ensure_base_table(&ep, &token);

    let mut failures = Vec::new();

    for (label, input_sql, cleanup_sql) in FIX_MATRIX {
        // Pre-cleanup: remove leftover objects from previous runs
        if !cleanup_sql.is_empty() {
            cleanup(&ep, &token, cleanup_sql);
        }

        let result = fix_sql(input_sql);
        let fixed = &result.sql;

        if fixed.is_empty() {
            continue;
        }

        let exec_result = if fixed.contains(";\n") {
            run_sql_file(&ep, &token, fixed)
        } else {
            run_sql(&ep, &token, fixed)
        };

        if let Err(err) = exec_result {
            failures.push(format!(
                "[{label}]\n  Input:  {input_sql}\n  Fixed:  {fixed}\n  Error:  {err}"
            ));
        }

        if !cleanup_sql.is_empty() {
            cleanup(&ep, &token, cleanup_sql);
        }
    }

    cleanup(&ep, &token, "DROP TABLE IF EXISTS _clust_base CASCADE;");

    assert!(
        failures.is_empty(),
        "Fix matrix failures against DSQL cluster:\n\n{}",
        failures.join("\n\n")
    );
}

// ═══════════════════════════════════════════════════════════════════════
// 2. MULTI-STATEMENT FIX VALIDATION
// ═══════════════════════════════════════════════════════════════════════

#[test]
#[ignore = "requires DSQL cluster — run via `cargo test --ignored` with DSQL_ENDPOINT set"]
fn fix_multi_statement_against_cluster() {
    let ep = endpoint();
    let region = region();
    let token = generate_token(&ep, &region);

    cleanup(&ep, &token, "DROP INDEX IF EXISTS _clust_multi_idx;");
    cleanup(&ep, &token, "DROP TABLE IF EXISTS _clust_multi;");

    let input = "CREATE TABLE _clust_multi (id SERIAL PRIMARY KEY);\nCREATE INDEX _clust_multi_idx ON _clust_multi(id);";
    let result = fix_sql(input);

    let exec_result = run_sql_file(&ep, &token, &result.sql);
    cleanup(&ep, &token, "DROP INDEX IF EXISTS _clust_multi_idx;");
    cleanup(&ep, &token, "DROP TABLE IF EXISTS _clust_multi;");

    assert!(
        exec_result.is_ok(),
        "Multi-statement fix failed:\n  Input: {input}\n  Fixed: {}\n  Error: {}",
        result.sql,
        exec_result.unwrap_err()
    );
}

// ═══════════════════════════════════════════════════════════════════════
// 3. CLEAN SQL VALIDATION — types we say are supported must actually work
// ═══════════════════════════════════════════════════════════════════════
// Uses the shared SUPPORTED_TYPES list from tests/common/mod.rs so that
// the unit and cluster type lists can't drift.

#[test]
#[ignore = "requires DSQL cluster — run via `cargo test --ignored` with DSQL_ENDPOINT set"]
fn clean_types_accepted_by_cluster() {
    let ep = endpoint();
    let region = region();
    let token = generate_token(&ep, &region);

    let mut failures = Vec::new();

    for (label, col_type) in common::SUPPORTED_TYPES {
        let tbl = format!("_clust_type_{}", label.replace('-', "_"));
        let drop = format!("DROP TABLE IF EXISTS {tbl};");
        cleanup(&ep, &token, &drop);

        let sql = format!("CREATE TABLE {tbl} (col {col_type});");
        if let Err(err) = run_sql(&ep, &token, &sql) {
            failures.push(format!(
                "[{label}] {col_type}\n  SQL: {sql}\n  Error: {err}"
            ));
        }
        cleanup(&ep, &token, &drop);
    }

    assert!(
        failures.is_empty(),
        "Clean type matrix failures against DSQL cluster:\n\n{}",
        failures.join("\n\n")
    );
}

// ═══════════════════════════════════════════════════════════════════════
// 4. CLEAN STATEMENTS — shared with unit tests
// ═══════════════════════════════════════════════════════════════════════
// Uses the shared list from tests/common/mod.rs so that every statement
// the linter passes as "clean" is also validated on a real DSQL cluster.

#[test]
#[ignore = "requires DSQL cluster — run via `cargo test --ignored` with DSQL_ENDPOINT set"]
fn clean_statements_accepted_by_cluster() {
    let ep = endpoint();
    let region = region();
    let token = generate_token(&ep, &region);

    cleanup(&ep, &token, "DROP TABLE IF EXISTS _clean_base CASCADE;");
    run_sql(&ep, &token, "CREATE TABLE _clean_base (id INT, name TEXT);").expect("setup failed");

    let mut failures = Vec::new();

    for (label, sql, setup_sql, cleanup_sql) in common::CLEAN_STATEMENTS {
        if !setup_sql.is_empty() {
            run_cleanup_stmts(&ep, &token, setup_sql);
        }

        if let Err(err) = run_sql(&ep, &token, sql) {
            failures.push(format!("[{label}] {sql}\n  Error: {err}"));
        }

        if !cleanup_sql.is_empty() {
            run_cleanup_stmts(&ep, &token, cleanup_sql);
        }
    }

    cleanup(&ep, &token, "DROP TABLE IF EXISTS _clean_base CASCADE;");

    assert!(
        failures.is_empty(),
        "Clean statement failures against DSQL cluster:\n\n{}",
        failures.join("\n\n")
    );
}

// ═══════════════════════════════════════════════════════════════════════
// 5. INDEX CREATION METHODS — verify ASYNC works, CREATE INDEX ASYNC
//    with UNIQUE, IF NOT EXISTS, etc.
// ═══════════════════════════════════════════════════════════════════════

#[test]
#[ignore = "requires DSQL cluster — run via `cargo test --ignored` with DSQL_ENDPOINT set"]
fn index_variants_accepted_by_cluster() {
    let ep = endpoint();
    let region = region();
    let token = generate_token(&ep, &region);

    cleanup(&ep, &token, "DROP TABLE IF EXISTS _clust_idxtbl CASCADE;");
    run_sql(
        &ep,
        &token,
        "CREATE TABLE _clust_idxtbl (id INT, name TEXT, val INT);",
    )
    .expect("setup failed");

    let cases: &[(&str, &str, &str)] = &[
        (
            "async-basic",
            "CREATE INDEX ASYNC _clust_ix1 ON _clust_idxtbl(id);",
            "DROP INDEX IF EXISTS _clust_ix1;",
        ),
        (
            "async-unique",
            "CREATE UNIQUE INDEX ASYNC _clust_ix2 ON _clust_idxtbl(val);",
            "DROP INDEX IF EXISTS _clust_ix2;",
        ),
        (
            "async-multi-col",
            "CREATE INDEX ASYNC _clust_ix3 ON _clust_idxtbl(id, name);",
            "DROP INDEX IF EXISTS _clust_ix3;",
        ),
        (
            "async-if-not-exists",
            "CREATE INDEX ASYNC IF NOT EXISTS _clust_ix4 ON _clust_idxtbl(id);",
            "DROP INDEX IF EXISTS _clust_ix4;",
        ),
    ];

    let mut failures = Vec::new();
    for (label, sql, cleanup_sql) in cases {
        cleanup(&ep, &token, cleanup_sql);
        if let Err(err) = run_sql(&ep, &token, sql) {
            failures.push(format!("[{label}] {sql}\n  Error: {err}"));
        }
        cleanup(&ep, &token, cleanup_sql);
    }

    cleanup(&ep, &token, "DROP TABLE IF EXISTS _clust_idxtbl CASCADE;");

    assert!(
        failures.is_empty(),
        "Index variant failures:\n\n{}",
        failures.join("\n\n")
    );
}

// ═══════════════════════════════════════════════════════════════════════
// 6. SEQUENCE VARIANTS
// ═══════════════════════════════════════════════════════════════════════

#[test]
#[ignore = "requires DSQL cluster — run via `cargo test --ignored` with DSQL_ENDPOINT set"]
fn sequence_variants_accepted_by_cluster() {
    let ep = endpoint();
    let region = region();
    let token = generate_token(&ep, &region);

    let cases: &[(&str, &str, &str)] = &[
        (
            "bigint-cache1",
            "CREATE SEQUENCE _clust_s1 AS BIGINT CACHE 1;",
            "DROP SEQUENCE IF EXISTS _clust_s1;",
        ),
        (
            "bigint-cache65536",
            "CREATE SEQUENCE _clust_s2 AS BIGINT CACHE 65536;",
            "DROP SEQUENCE IF EXISTS _clust_s2;",
        ),
        (
            "default-cache1",
            "CREATE SEQUENCE _clust_s3 CACHE 1;",
            "DROP SEQUENCE IF EXISTS _clust_s3;",
        ),
        (
            "with-increment",
            "CREATE SEQUENCE _clust_s4 CACHE 1 INCREMENT 1;",
            "DROP SEQUENCE IF EXISTS _clust_s4;",
        ),
    ];

    let mut failures = Vec::new();
    for (label, sql, cleanup_sql) in cases {
        cleanup(&ep, &token, cleanup_sql);
        if let Err(err) = run_sql(&ep, &token, sql) {
            failures.push(format!("[{label}] {sql}\n  Error: {err}"));
        }
        cleanup(&ep, &token, cleanup_sql);
    }

    assert!(
        failures.is_empty(),
        "Sequence variant failures:\n\n{}",
        failures.join("\n\n")
    );
}

// ═══════════════════════════════════════════════════════════════════════
// 7. CLEAN MULTI-STATEMENT CASES — shared with unit tests
// ═══════════════════════════════════════════════════════════════════════
// Validates that every multi-statement SQL pattern the linter passes as
// "clean" (no DDL transaction errors) actually executes on DSQL.
// Uses the shared list from tests/common/mod.rs.

#[test]
#[ignore = "requires DSQL cluster — run via `cargo test --ignored` with DSQL_ENDPOINT set"]
fn clean_multi_statement_cases_accepted_by_cluster() {
    let ep = endpoint();
    let region = region();
    let token = generate_token(&ep, &region);

    let mut failures = Vec::new();

    for (label, sql, cleanup_sql) in common::CLEAN_MULTI_STATEMENT_CASES {
        run_cleanup_stmts(&ep, &token, cleanup_sql);

        if let Err(err) = run_sql_file(&ep, &token, sql) {
            failures.push(format!("[{label}]\n  SQL: {sql}\n  Error: {err}"));
        }

        run_cleanup_stmts(&ep, &token, cleanup_sql);
    }

    assert!(
        failures.is_empty(),
        "Clean multi-statement failures against DSQL cluster:\n\n{}",
        failures.join("\n\n")
    );
}

// ═══════════════════════════════════════════════════════════════════════
// 8. LINT RULE COVERAGE — compiler-enforced via LintRule enum
// ═══════════════════════════════════════════════════════════════════════
// Iterates every LintRule variant via cluster_test_for_rule (exhaustive
// match — new variant without arm = compile error). For fixable rules,
// runs fix_sql and executes the result on the cluster. Unfixable rules
// are validated by the unit test in integration_test.rs.

#[test]
#[ignore = "requires DSQL cluster — run via `cargo test --ignored` with DSQL_ENDPOINT set"]
fn lint_rule_fixes_execute_on_cluster() {
    use dsql_lint::FixResult;

    let ep = endpoint();
    let region = region();
    let token = generate_token(&ep, &region);

    cleanup(&ep, &token, "DROP TABLE IF EXISTS _clust_base CASCADE;");
    ensure_base_table(&ep, &token);

    let mut failures = Vec::new();

    for rule in LintRule::iter() {
        let Some((sql, _expected_msg)) = common::cluster_test_for_rule(rule) else {
            continue;
        };

        let result = fix_sql(sql);

        let has_unfixable = result
            .diagnostics
            .iter()
            .any(|d| matches!(d.fix_result, FixResult::Unfixable));
        if has_unfixable || result.sql.is_empty() {
            continue;
        }

        // Clean up objects that might exist from a previous run
        cleanup(&ep, &token, "DROP TABLE IF EXISTS _r CASCADE;");
        cleanup(&ep, &token, "DROP TABLE IF EXISTS _r_a CASCADE;");
        cleanup(&ep, &token, "DROP TABLE IF EXISTS _r_b CASCADE;");
        cleanup(&ep, &token, "DROP SEQUENCE IF EXISTS _r_seq;");
        cleanup(&ep, &token, "DROP INDEX IF EXISTS _r_idx;");

        let exec_result = if result.sql.contains(";\n") {
            run_sql_file(&ep, &token, &result.sql)
        } else {
            run_sql(&ep, &token, &result.sql)
        };

        if let Err(err) = exec_result {
            failures.push(format!(
                "[{rule:?}]\n  Input: {sql}\n  Fixed: {}\n  Error: {err}",
                result.sql
            ));
        }

        // Clean up
        cleanup(&ep, &token, "DROP TABLE IF EXISTS _r CASCADE;");
        cleanup(&ep, &token, "DROP TABLE IF EXISTS _r_a CASCADE;");
        cleanup(&ep, &token, "DROP TABLE IF EXISTS _r_b CASCADE;");
        cleanup(&ep, &token, "DROP SEQUENCE IF EXISTS _r_seq;");
        cleanup(&ep, &token, "DROP INDEX IF EXISTS _r_idx;");
    }

    cleanup(&ep, &token, "DROP TABLE IF EXISTS _clust_base CASCADE;");

    assert!(
        failures.is_empty(),
        "LintRule fix-and-execute failures against DSQL cluster:\n\n{}",
        failures.join("\n\n")
    );
}

// ═══════════════════════════════════════════════════════════════════════
// 9. DDL TRANSACTION FIX VALIDATION
// ═══════════════════════════════════════════════════════════════════════
// Multi-DDL transaction inputs → fix_sql → execute on DSQL.
// Validates the split output is actually valid DSQL.

const DDL_TXN_FIX_CASES: &[(&str, &str, &str)] = &[
    (
        "two-ddl-in-txn",
        "BEGIN;\nCREATE TABLE _clust_txn_fix_a (id INT);\nCREATE TABLE _clust_txn_fix_b (id INT);\nCOMMIT;",
        "DROP TABLE IF EXISTS _clust_txn_fix_a; DROP TABLE IF EXISTS _clust_txn_fix_b;",
    ),
    (
        "three-ddl-in-txn",
        "BEGIN;\nCREATE TABLE _clust_txn_fix_c (id INT);\nCREATE TABLE _clust_txn_fix_d (id INT);\nCREATE TABLE _clust_txn_fix_e (id INT);\nCOMMIT;",
        "DROP TABLE IF EXISTS _clust_txn_fix_c; DROP TABLE IF EXISTS _clust_txn_fix_d; DROP TABLE IF EXISTS _clust_txn_fix_e;",
    ),
    (
        "ddl-with-dml-in-txn",
        "BEGIN;\nCREATE TABLE _clust_txn_fix_f (id INT);\nINSERT INTO _clust_txn_fix_f VALUES (1);\nCREATE TABLE _clust_txn_fix_g (id INT);\nCOMMIT;",
        "DROP TABLE IF EXISTS _clust_txn_fix_f; DROP TABLE IF EXISTS _clust_txn_fix_g;",
    ),
    (
        "nested-begin-in-txn",
        "BEGIN;\nCREATE TABLE _clust_txn_fix_h (id INT);\nBEGIN;\nCREATE TABLE _clust_txn_fix_i (id INT);\nCOMMIT;",
        "DROP TABLE IF EXISTS _clust_txn_fix_h; DROP TABLE IF EXISTS _clust_txn_fix_i;",
    ),
    (
        "serial-fix-plus-split",
        "BEGIN;\nCREATE TABLE _clust_txn_fix_j (id SERIAL PRIMARY KEY);\nCREATE TABLE _clust_txn_fix_k (id INT);\nCOMMIT;",
        "DROP TABLE IF EXISTS _clust_txn_fix_j; DROP TABLE IF EXISTS _clust_txn_fix_k;",
    ),
];

#[test]
#[ignore = "requires DSQL cluster — run via `cargo test --ignored` with DSQL_ENDPOINT set"]
fn ddl_transaction_fix_against_cluster() {
    let ep = endpoint();
    let region = region();
    let token = generate_token(&ep, &region);

    let mut failures = Vec::new();

    for (label, input_sql, cleanup_sql) in DDL_TXN_FIX_CASES {
        run_cleanup_stmts(&ep, &token, cleanup_sql);

        let result = fix_sql(input_sql);

        if let Err(err) = run_sql_file(&ep, &token, &result.sql) {
            failures.push(format!(
                "[{label}]\n  Input:  {input_sql}\n  Fixed:  {}\n  Error:  {err}",
                result.sql
            ));
        }

        run_cleanup_stmts(&ep, &token, cleanup_sql);
    }

    assert!(
        failures.is_empty(),
        "DDL transaction fix failures against DSQL cluster:\n\n{}",
        failures.join("\n\n")
    );
}
