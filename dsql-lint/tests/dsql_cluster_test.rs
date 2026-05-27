//! DSQL cluster integration tests.
//!
//! These tests validate that:
//!   1. Every fixable SQL pattern, after `fix_sql`, executes successfully on a real DSQL cluster
//!   2. Every SQL pattern we pass through as "clean" actually executes on DSQL
//!
//! Gated behind the `dsql_cluster` cfg so `cargo test` skips them locally.
//! CI runs them via `RUSTFLAGS='--cfg dsql_cluster'` with `DSQL_ENDPOINT` set.
//!
//! To run locally:
//!   DSQL_ENDPOINT=<host> RUSTFLAGS='--cfg dsql_cluster' cargo test
//!
//! Prerequisites: `aws` CLI and `psql` in PATH, valid AWS credentials.
#![cfg(dsql_cluster)]

mod common;

use std::process::Command;
use std::sync::{Mutex, OnceLock};
use std::thread;
use std::time::Duration;

use dsql_lint::{fix_sql, lint_sql, FixResult, LintRule};
use strum::IntoEnumIterator;

const MAX_RETRIES: usize = 5;
// Per-fixture retry cap for `lint_rule_fixtures_validated_on_cluster`. Bounded
// tighter than `MAX_RETRIES` because each retry resets and re-runs setup, so
// the worst-case wall time per fixture grows linearly with this number.
const FIXTURE_MAX_RETRIES: usize = 3;
const RETRY_BASE_MS: u64 = 2000;
// Token lifetime in seconds requested from `aws dsql generate-db-connect-admin-auth-token`.
// The CLI default is 900s (15 min); the parallelized fixture loop plus the other tests can
// outrun that, so request a 1h token and reuse it across tests via `cluster_creds()`.
const TOKEN_EXPIRY_SECS: &str = "3600";

fn endpoint() -> String {
    std::env::var("DSQL_ENDPOINT").expect("DSQL_ENDPOINT must be set to a DSQL cluster hostname")
}

fn region() -> String {
    std::env::var("DSQL_REGION").unwrap_or_else(|_| "us-east-1".to_string())
}

/// Returns `(endpoint, token)` reused across all tests in this binary, so we don't pay
/// the AWS-CLI roundtrip per `#[test]`. Returned as owned `String`s so existing call
/// sites that take `&ep`/`&token` keep working without auto-deref noise.
///
/// **Use [`locked_creds`] instead** unless your test provides its own isolation
/// (e.g. a per-test schema). Tests that touch unqualified `public` objects
/// (`_clust_base`, `_clean_base`, …) must run serially; calling bare
/// `cluster_creds` from such a test would race against other tests and trigger
/// OC001 storms. `lint_rule_fixtures_validated_on_cluster` is the sole intended
/// caller — it runs its own per-worker schemas and deliberately bypasses the
/// shared lock.
fn cluster_creds() -> (String, String) {
    static CREDS: OnceLock<(String, String)> = OnceLock::new();
    CREDS
        .get_or_init(|| {
            let ep = endpoint();
            let region = region();
            let token = generate_token(&ep, &region);
            (ep, token)
        })
        .clone()
}

/// Serializes tests that share unqualified objects in `public` (`_clust_base`,
/// `_clean_base`, …). `lint_rule_fixtures_validated_on_cluster` runs its own
/// workers in per-worker schemas and does **not** take this lock. Rust's test
/// harness otherwise runs `#[test]`s in parallel and concurrent DDL on the same
/// `public._clust_base` raises OC001s faster than retries can absorb them.
fn shared_public_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

/// Acquire the `public`-schema lock and the cached cluster creds in one call.
/// `unwrap_or_else(|e| e.into_inner())` recovers from poisoning so a panicked
/// earlier test doesn't cascade through every later test as a `PoisonError`
/// that hides the real assertion.
fn locked_creds() -> (std::sync::MutexGuard<'static, ()>, String, String) {
    let guard = shared_public_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let (ep, token) = cluster_creds();
    (guard, ep, token)
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
            "--expires-in",
            TOKEN_EXPIRY_SECS,
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
    psql_cmd_in_schema(endpoint, token, None)
}

/// `search_path` lets parallel workers in `lint_rule_fixtures_validated_on_cluster`
/// reuse the same fixture text — references to `_clust_base`, `_r`, `_rej_*` resolve
/// to per-worker scratch objects in the worker's schema instead of stomping on each
/// other in `public`.
fn psql_cmd_in_schema(endpoint: &str, token: &str, schema: Option<&str>) -> Command {
    let mut cmd = Command::new("psql");
    cmd.env("PGHOST", endpoint)
        .env("PGPORT", "5432")
        .env("PGUSER", "admin")
        .env("PGPASSWORD", token)
        .env("PGDATABASE", "postgres")
        .env("PGSSLMODE", "require");
    if let Some(s) = schema {
        cmd.env("PGOPTIONS", format!("-c search_path={s},public"));
    }
    cmd
}

fn run_sql_in_schema(
    endpoint: &str,
    token: &str,
    schema: &str,
    sql: &str,
) -> Result<String, String> {
    for attempt in 0..MAX_RETRIES {
        let output = psql_cmd_in_schema(endpoint, token, Some(schema))
            .args(["-c", sql])
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
    Err("run_sql_in_schema: retry loop exited without result".into())
}

/// Run a multi-statement file once. Multi-DDL fixes leave partial state on OC001,
/// so retrying the same file in-place would hit "relation already exists". Callers
/// that need retries should drop+recreate their scratch schema between attempts
/// (see the fix-path retry loop in `lint_rule_fixtures_validated_on_cluster`).
fn run_sql_file_in_schema_once(
    endpoint: &str,
    token: &str,
    schema: &str,
    sql: &str,
) -> Result<String, String> {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("test.sql");
    std::fs::write(&path, sql).unwrap();
    let output = psql_cmd_in_schema(endpoint, token, Some(schema))
        .args(["-v", "ON_ERROR_STOP=1", "-f", path.to_str().unwrap()])
        .output()
        .expect("failed to run `psql`");
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    } else {
        Err(String::from_utf8_lossy(&output.stderr).to_string())
    }
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
    Err("run_sql: retry loop exited without result (MAX_RETRIES=0?)".into())
}

/// Runs a multi-statement SQL file on the cluster.
///
/// On OC001, multi-DDL files leave partial state (some `CREATE TABLE`s
/// committed, later ones rolled back). Re-running the same file would fail
/// with "relation already exists", so callers pass a `cleanup_sql` that the
/// retry loop applies *between* attempts to wipe partial state.
fn run_sql_file(
    endpoint: &str,
    token: &str,
    sql: &str,
    cleanup_sql: &str,
) -> Result<String, String> {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("test.sql");
    std::fs::write(&path, sql).unwrap();
    for attempt in 0..MAX_RETRIES {
        // ON_ERROR_STOP=1 makes psql exit non-zero on the first SQL error;
        // without it, multi-statement scripts always exit 0, causing
        // false-positive "succeeded" verdicts for cluster validation.
        let output = psql_cmd(endpoint, token)
            .args(["-v", "ON_ERROR_STOP=1", "-f", path.to_str().unwrap()])
            .output()
            .expect("failed to run `psql`");
        if output.status.success() {
            return Ok(String::from_utf8_lossy(&output.stdout).to_string());
        }
        let err = String::from_utf8_lossy(&output.stderr).to_string();
        if err.contains("OC001") && attempt < MAX_RETRIES - 1 {
            thread::sleep(Duration::from_millis(RETRY_BASE_MS * (attempt as u64 + 1)));
            if !cleanup_sql.is_empty() {
                run_cleanup_stmts(endpoint, token, cleanup_sql);
            }
            continue;
        }
        return Err(err);
    }
    Err("run_sql_file: retry loop exited without result (MAX_RETRIES=0?)".into())
}

fn cleanup(endpoint: &str, token: &str, sql: &str) {
    for attempt in 0..MAX_RETRIES {
        match run_sql_once(endpoint, token, sql) {
            Ok(_) => return,
            Err(err) if err.contains("OC001") && attempt < MAX_RETRIES - 1 => {
                thread::sleep(Duration::from_millis(RETRY_BASE_MS * (attempt as u64 + 1)));
            }
            Err(err) => {
                // Cleanup failures are non-fatal (the next test's setup may
                // succeed anyway), but silently swallowing them turns a real
                // failure ("relation already exists" on next attempt) into a
                // misleading error. Surface them so a flaky cluster is debuggable.
                eprintln!("WARN: cleanup `{sql}` failed: {err}");
                return;
            }
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
];

#[test]
fn fix_matrix_against_cluster() {
    let (_shared, ep, token) = locked_creds();
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

        // FIX_MATRIX is the "fixable → executes clean" matrix. An Unfixable
        // diagnostic means fix_sql returned the input unchanged, which would
        // be (correctly) rejected by the cluster — covered elsewhere by
        // `lint_rule_fixtures_validated_on_cluster`. Such entries don't
        // belong here.
        let has_unfixable = result
            .diagnostics
            .iter()
            .any(|d| matches!(d.fix_result, FixResult::Unfixable));
        if has_unfixable {
            failures.push(format!(
                "[{label}] entry produced Unfixable diagnostics — does not belong in FIX_MATRIX\n  Input: {input_sql}"
            ));
            if !cleanup_sql.is_empty() {
                cleanup(&ep, &token, cleanup_sql);
            }
            continue;
        }

        let exec_result = if fixed.contains(";\n") {
            run_sql_file(&ep, &token, fixed, cleanup_sql)
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
fn fix_multi_statement_against_cluster() {
    let (_shared, ep, token) = locked_creds();

    cleanup(&ep, &token, "DROP INDEX IF EXISTS _clust_multi_idx;");
    cleanup(&ep, &token, "DROP TABLE IF EXISTS _clust_multi;");

    let input = "CREATE TABLE _clust_multi (id SERIAL PRIMARY KEY);\nCREATE INDEX _clust_multi_idx ON _clust_multi(id);";
    let result = fix_sql(input);

    let multi_cleanup = "DROP INDEX IF EXISTS _clust_multi_idx; DROP TABLE IF EXISTS _clust_multi;";
    let exec_result = run_sql_file(&ep, &token, &result.sql, multi_cleanup);
    run_cleanup_stmts(&ep, &token, multi_cleanup);

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
fn clean_types_accepted_by_cluster() {
    let (_shared, ep, token) = locked_creds();

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
fn clean_statements_accepted_by_cluster() {
    let (_shared, ep, token) = locked_creds();

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
fn index_variants_accepted_by_cluster() {
    let (_shared, ep, token) = locked_creds();

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
fn sequence_variants_accepted_by_cluster() {
    let (_shared, ep, token) = locked_creds();

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
fn clean_multi_statement_cases_accepted_by_cluster() {
    let (_shared, ep, token) = locked_creds();

    let mut failures = Vec::new();

    for (label, sql, cleanup_sql) in common::CLEAN_MULTI_STATEMENT_CASES {
        run_cleanup_stmts(&ep, &token, cleanup_sql);

        if let Err(err) = run_sql_file(&ep, &token, sql, cleanup_sql) {
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
// Iterates every LintRule variant via fixture_for_rule (exhaustive match —
// new variant without arm = compile error). For each rule:
//   • The unfixed input must be rejected by DSQL (proves the rule is needed)
//   • If fix_sql produces non-Unfixable output, the fixed SQL must run clean

/// Number of parallel workers in `lint_rule_fixtures_validated_on_cluster`.
///
/// Each worker holds its own DSQL schema reused across iterations. DSQL caps
/// total schemas at 10, so unique-per-iteration is not viable and the pool
/// must stay small. Reset between iterations is serialized via `reset_lock`
/// to keep `DROP SCHEMA CASCADE` (multi-row catalog mutation, the dominant
/// OC001 source) from contending with itself across workers.
const RULE_FIXTURE_WORKERS: usize = 3;

#[test]
fn lint_rule_fixtures_validated_on_cluster() {
    let (ep, token) = cluster_creds();

    let rules: Vec<LintRule> = LintRule::iter()
        .filter(|r| common::fixture_for_rule(*r).is_some())
        .collect();

    let queue: Mutex<Vec<LintRule>> = Mutex::new(rules);
    let failures: Mutex<Vec<String>> = Mutex::new(Vec::new());
    // Serializes the reset step (DROP + CREATE schema + CREATE base table)
    // across workers. Reset is the only metadata-mutating step in the loop;
    // the rest (fixture setup, lint, fix, exec) stays parallel.
    let reset_lock: Mutex<()> = Mutex::new(());
    let pid = std::process::id();

    thread::scope(|s| {
        for wid in 0..RULE_FIXTURE_WORKERS {
            let ep = ep.as_str();
            let token = token.as_str();
            let queue = &queue;
            let failures = &failures;
            let reset_lock = &reset_lock;
            s.spawn(move || {
                let schema = format!("rule_{pid}_w{wid}");

                let reset = || -> Result<(), String> {
                    let _g = reset_lock.lock().unwrap_or_else(|e| e.into_inner());
                    run_sql(ep, token, &format!("DROP SCHEMA IF EXISTS {schema} CASCADE;"))?;
                    run_sql(ep, token, &format!("CREATE SCHEMA {schema};"))?;
                    run_sql_in_schema(
                        ep,
                        token,
                        &schema,
                        "CREATE TABLE _clust_base (id INT, col INT);",
                    )?;
                    Ok(())
                };

                if let Err(err) = reset() {
                    failures
                        .lock()
                        .unwrap()
                        .push(format!("[worker {wid}] schema setup failed: {err}"));
                    return;
                }

                let exec = |schema: &str, sql: &str| {
                    if sql.contains(";\n") {
                        run_sql_file_in_schema_once(ep, token, schema, sql)
                    } else {
                        run_sql_in_schema(ep, token, schema, sql)
                    }
                };

                loop {
                    let rule = match queue.lock().unwrap().pop() {
                        Some(r) => r,
                        None => break,
                    };
                    let fix = common::fixture_for_rule(rule).expect("filtered above");

                    let diags = lint_sql(fix.sql);
                    if !diags.iter().any(|d| d.rule == rule) {
                        failures.lock().unwrap().push(format!(
                            "[{rule:?}] fixture does not produce a `{rule:?}` diagnostic\n  Input: {}\n  Got: {diags:?}",
                            fix.sql
                        ));
                        continue;
                    }

                    if let Err(err) = reset() {
                        failures
                            .lock()
                            .unwrap()
                            .push(format!("[{rule:?}] reset failed: {err}"));
                        continue;
                    }
                    if !fix.setup_sql.is_empty() {
                        if let Err(err) = run_sql_in_schema(ep, token, &schema, fix.setup_sql) {
                            failures.lock().unwrap().push(format!(
                                "[{rule:?}] setup failed\n  Setup: {}\n  Error: {err}",
                                fix.setup_sql
                            ));
                            continue;
                        }
                    }

                    // Unfixed SQL is expected to be rejected by DSQL today.
                    if exec(&schema, fix.sql).is_ok() {
                        failures.lock().unwrap().push(format!(
                            "[{rule:?}] expected DSQL to reject unfixed input, but it succeeded. \
                             Check if DSQL now supports this feature — if so, remove the rule. \
                             Otherwise the fixture's setup may have masked the rejection (e.g. the \
                             referenced object exists when it shouldn't, or vice versa).\n  Input: {}",
                            fix.sql
                        ));
                    }

                    let result = fix_sql(fix.sql);
                    // Filter to the rule under test so an unrelated `Unfixable`
                    // diagnostic (e.g. a fixture that incidentally trips another
                    // rule) doesn't silently skip fix-path validation for *this*
                    // rule. Without the filter, broadening any fixture in a way
                    // that adds a secondary Unfixable would disable cluster
                    // verification for the rule the fixture is meant to test.
                    let has_unfixable = result
                        .diagnostics
                        .iter()
                        .filter(|d| d.rule == rule)
                        .any(|d| matches!(d.fix_result, FixResult::Unfixable));
                    if has_unfixable || result.sql.is_empty() {
                        continue;
                    }

                    if let Err(err) = reset() {
                        failures
                            .lock()
                            .unwrap()
                            .push(format!("[{rule:?}] fix-path reset failed: {err}"));
                        continue;
                    }
                    if !fix.setup_sql.is_empty() {
                        if let Err(err) = run_sql_in_schema(ep, token, &schema, fix.setup_sql) {
                            failures.lock().unwrap().push(format!(
                                "[{rule:?}] fix-path setup failed\n  Setup: {}\n  Error: {err}",
                                fix.setup_sql
                            ));
                            continue;
                        }
                    }

                    // Multi-statement fix paths can leave partial state when an
                    // OC001 hits mid-file; reset between retries so each attempt
                    // starts clean.
                    let mut last_err = None;
                    for retry in 0..FIXTURE_MAX_RETRIES {
                        if retry > 0 {
                            if let Err(e) = reset() {
                                last_err =
                                    Some(format!("reset before retry {retry} failed: {e}"));
                                break;
                            }
                            if !fix.setup_sql.is_empty() {
                                if let Err(e) =
                                    run_sql_in_schema(ep, token, &schema, fix.setup_sql)
                                {
                                    last_err = Some(format!(
                                        "setup before retry {retry} failed: {e}"
                                    ));
                                    break;
                                }
                            }
                        }
                        match exec(&schema, &result.sql) {
                            Ok(_) => {
                                last_err = None;
                                break;
                            }
                            Err(e) if e.contains("OC001") => {
                                last_err = Some(e);
                                continue;
                            }
                            Err(e) => {
                                last_err = Some(e);
                                break;
                            }
                        }
                    }
                    if let Some(err) = last_err {
                        failures.lock().unwrap().push(format!(
                            "[{rule:?}]\n  Input: {}\n  Fixed: {}\n  Error: {err}",
                            fix.sql, result.sql
                        ));
                    }
                }

                // Best-effort cleanup; cluster gets torn down after CI anyway.
                let _g = reset_lock.lock().unwrap_or_else(|e| e.into_inner());
                if let Err(err) =
                    run_sql(ep, token, &format!("DROP SCHEMA IF EXISTS {schema} CASCADE;"))
                {
                    eprintln!("WARN: post-test DROP SCHEMA {schema} failed: {err}");
                }
            });
        }
    });

    let mut failures = failures.into_inner().unwrap();
    // Workers process the rule queue in nondeterministic order; sort so the
    // failure message diff is stable across runs.
    failures.sort();
    assert!(
        failures.is_empty(),
        "LintRule fixture failures against DSQL cluster:\n\n{}",
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
fn ddl_transaction_fix_against_cluster() {
    let (_shared, ep, token) = locked_creds();

    let mut failures = Vec::new();

    for (label, input_sql, cleanup_sql) in DDL_TXN_FIX_CASES {
        run_cleanup_stmts(&ep, &token, cleanup_sql);

        let result = fix_sql(input_sql);

        if let Err(err) = run_sql_file(&ep, &token, &result.sql, cleanup_sql) {
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
