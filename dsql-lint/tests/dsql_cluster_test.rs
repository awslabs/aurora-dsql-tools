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
//!
//! ## Isolation model
//!
//! Each `#[test]` owns a per-test DSQL schema, created in `ClusterScope::new`
//! and dropped in `Drop`. All SQL routes through that schema via
//! `PGOPTIONS=-c search_path=…`, so unqualified table names like `_clust_base`
//! resolve to the test's own schema. The cargo harness can run all tests in
//! parallel — there is no shared `public` state and no process-wide lock.
//!
//! OC001 (schema-version conflict) is *not* schema-scoped per DSQL docs: any
//! catalog mutation anywhere bumps the cluster-wide catalog version. The
//! `with_oc001_retry` helper absorbs these for single-statement operations.
//! Multi-statement files cannot be retried in place (partial state from a
//! half-applied file would re-fail with "already exists"); callers wrap
//! `exec_file_once` in their own loop with `cx.reset()` between attempts.
#![cfg(dsql_cluster)]

mod common;

use std::process::Command;
use std::sync::OnceLock;
use std::thread;
use std::time::Duration;

use dsql_lint::{fix_sql, lint_sql, FixResult, LintRule};
use strum::IntoEnumIterator;

const OC001_MAX_RETRIES: usize = 5;
const OC001_BASE_DELAY_MS: u64 = 100;
// Per-fixture retry cap for the multi-DDL fix path in
// `lint_rule_fixtures_validated_on_cluster`. Each retry resets the schema and
// re-runs setup, so the worst-case wall time per fixture grows linearly.
const FIXTURE_FIX_PATH_MAX_RETRIES: usize = 3;
// Token lifetime in seconds requested from `aws dsql generate-db-connect-admin-auth-token`.
// The CLI default is 900s (15 min); the parallelized cluster suite can outrun
// that, so request a 1h token and reuse it via `cluster_creds()`.
const TOKEN_EXPIRY_SECS: &str = "3600";

fn endpoint() -> String {
    std::env::var("DSQL_ENDPOINT").expect("DSQL_ENDPOINT must be set to a DSQL cluster hostname")
}

fn region() -> String {
    std::env::var("DSQL_REGION").unwrap_or_else(|_| "us-east-1".to_string())
}

/// Returns `(endpoint, token)` cached across all tests in this binary so we
/// don't pay the AWS-CLI roundtrip per `#[test]`.
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

/// Runs `op` and retries on OC001 with linear backoff (100/200/300/400ms).
/// All other errors propagate immediately. The final OC001 returns the error
/// to the caller rather than retrying again.
fn with_oc001_retry<F, T>(mut op: F) -> Result<T, String>
where
    F: FnMut() -> Result<T, String>,
{
    for attempt in 0..OC001_MAX_RETRIES {
        match op() {
            Ok(v) => return Ok(v),
            Err(e) if e.contains("OC001") && attempt < OC001_MAX_RETRIES - 1 => {
                thread::sleep(Duration::from_millis(
                    OC001_BASE_DELAY_MS * (attempt as u64 + 1),
                ));
            }
            Err(e) => return Err(e),
        }
    }
    unreachable!("loop returns on every iteration when attempts > 0")
}

fn psql_cmd(endpoint: &str, token: &str, schema: &str) -> Command {
    let mut cmd = Command::new("psql");
    cmd.env("PGHOST", endpoint)
        .env("PGPORT", "5432")
        .env("PGUSER", "admin")
        .env("PGPASSWORD", token)
        .env("PGDATABASE", "postgres")
        .env("PGSSLMODE", "require")
        .env("PGOPTIONS", format!("-c search_path={schema},public"));
    cmd
}

fn exec_one(endpoint: &str, token: &str, schema: &str, sql: &str) -> Result<String, String> {
    let output = psql_cmd(endpoint, token, schema)
        .args(["-c", sql])
        .output()
        .expect("failed to run `psql`");
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    } else {
        Err(String::from_utf8_lossy(&output.stderr).to_string())
    }
}

fn exec_file(endpoint: &str, token: &str, schema: &str, sql: &str) -> Result<String, String> {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("test.sql");
    std::fs::write(&path, sql).unwrap();
    // ON_ERROR_STOP=1 makes psql exit non-zero on the first SQL error;
    // without it, multi-statement scripts always exit 0, causing
    // false-positive "succeeded" verdicts for cluster validation.
    let output = psql_cmd(endpoint, token, schema)
        .args(["-v", "ON_ERROR_STOP=1", "-f", path.to_str().unwrap()])
        .output()
        .expect("failed to run `psql`");
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    } else {
        Err(String::from_utf8_lossy(&output.stderr).to_string())
    }
}

/// RAII handle for one test's isolated DSQL schema.
struct ClusterScope {
    ep: String,
    token: String,
    schema: String,
}

impl ClusterScope {
    /// Creates a fresh per-test schema. Drops any leftover state from a
    /// previously panicked run before recreating.
    fn new(name: &str) -> Self {
        let (ep, token) = cluster_creds();
        let schema = format!("t_{name}");
        let scope = Self { ep, token, schema };
        with_oc001_retry(|| {
            exec_one(
                &scope.ep,
                &scope.token,
                "public",
                &format!("DROP SCHEMA IF EXISTS {} CASCADE;", scope.schema),
            )
        })
        .expect("ClusterScope: failed to drop pre-existing schema");
        with_oc001_retry(|| {
            exec_one(
                &scope.ep,
                &scope.token,
                "public",
                &format!("CREATE SCHEMA {};", scope.schema),
            )
        })
        .expect("ClusterScope: failed to create schema");
        scope
    }

    /// Drops and recreates the schema. Used for partial-state recovery in
    /// the multi-DDL fix-path retry loop.
    fn reset(&self) -> Result<(), String> {
        with_oc001_retry(|| {
            exec_one(
                &self.ep,
                &self.token,
                "public",
                &format!("DROP SCHEMA IF EXISTS {} CASCADE;", self.schema),
            )
        })?;
        with_oc001_retry(|| {
            exec_one(
                &self.ep,
                &self.token,
                "public",
                &format!("CREATE SCHEMA {};", self.schema),
            )
        })?;
        Ok(())
    }

    /// Run a single SQL statement in this scope's schema. Retries on OC001.
    fn exec(&self, sql: &str) -> Result<String, String> {
        with_oc001_retry(|| exec_one(&self.ep, &self.token, &self.schema, sql))
    }

    /// Run a multi-statement file. **No automatic retry** — partial state
    /// from a half-applied file would re-fail with "already exists" on retry.
    /// Callers needing retry-with-recovery wrap this in their own loop and
    /// call `reset()` between attempts.
    fn exec_file_once(&self, sql: &str) -> Result<String, String> {
        exec_file(&self.ep, &self.token, &self.schema, sql)
    }

    /// Run a multi-statement file with OC001 retries, applying `cleanup_sql`
    /// between attempts to wipe partial state from a half-applied file.
    fn exec_file_retry(&self, sql: &str, cleanup_sql: &str) -> Result<String, String> {
        for attempt in 0..OC001_MAX_RETRIES {
            match exec_file(&self.ep, &self.token, &self.schema, sql) {
                Ok(v) => return Ok(v),
                Err(e) if e.contains("OC001") && attempt < OC001_MAX_RETRIES - 1 => {
                    thread::sleep(Duration::from_millis(
                        OC001_BASE_DELAY_MS * (attempt as u64 + 1),
                    ));
                    if !cleanup_sql.is_empty() {
                        run_cleanup_stmts(self, cleanup_sql);
                    }
                }
                Err(e) => return Err(e),
            }
        }
        unreachable!("loop returns on every iteration when attempts > 0")
    }

    /// Execute SQL whose statement count isn't known at the call site.
    /// `fix_sql` may turn a single-statement input into multi-statement output
    /// (e.g. SERIAL fix splits into CREATE TABLE + companion DDL), so the
    /// caller can't pick `exec` vs `exec_file_retry` upfront. Dispatches via
    /// `;\n` substring — the same heuristic the original code used.
    fn exec_auto(&self, sql: &str, cleanup_sql: &str) -> Result<String, String> {
        if sql.contains(";\n") {
            self.exec_file_retry(sql, cleanup_sql)
        } else {
            self.exec(sql)
        }
    }
}

impl Drop for ClusterScope {
    fn drop(&mut self) {
        // Best-effort cleanup — cluster gets torn down after CI anyway. We
        // log on failure so a flaky teardown is debuggable, but never panic
        // (panicking from Drop during a panic aborts the process).
        if let Err(err) = with_oc001_retry(|| {
            exec_one(
                &self.ep,
                &self.token,
                "public",
                &format!("DROP SCHEMA IF EXISTS {} CASCADE;", self.schema),
            )
        }) {
            eprintln!("WARN: drop schema {} failed: {err}", self.schema);
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════
// 1. FIX VALIDATION MATRIX
// ═══════════════════════════════════════════════════════════════════════
// Each entry: (label, unfixed_sql, cleanup_sql)
// cleanup_sql runs between cases to keep the scope's schema reusable
// across iterations (e.g. `_clust_base` mutations from `alter-*` cases).

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

/// Apply each statement in a `;`-separated cleanup string. Used between
/// FIX_MATRIX cases that mutate `_clust_base` so subsequent cases see a
/// pristine base table.
fn run_cleanup_stmts(cx: &ClusterScope, cleanup_sql: &str) {
    for stmt in cleanup_sql.split(';') {
        let stmt = stmt.trim();
        if !stmt.is_empty() {
            if let Err(err) = cx.exec(&format!("{stmt};")) {
                eprintln!("WARN: cleanup `{stmt}` failed: {err}");
            }
        }
    }
}

#[test]
fn fix_matrix_against_cluster() {
    let cx = ClusterScope::new("fix_matrix");
    cx.exec("CREATE TABLE _clust_base (id INT, col INT);")
        .expect("base table setup");

    let mut failures = Vec::new();

    for (label, input_sql, cleanup_sql) in FIX_MATRIX {
        if !cleanup_sql.is_empty() {
            run_cleanup_stmts(&cx, cleanup_sql);
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
                run_cleanup_stmts(&cx, cleanup_sql);
            }
            continue;
        }

        if let Err(err) = cx.exec_auto(fixed, cleanup_sql) {
            failures.push(format!(
                "[{label}]\n  Input:  {input_sql}\n  Fixed:  {fixed}\n  Error:  {err}"
            ));
        }

        if !cleanup_sql.is_empty() {
            run_cleanup_stmts(&cx, cleanup_sql);
        }
    }

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
    let cx = ClusterScope::new("fix_multi");

    let input = "CREATE TABLE _clust_multi (id SERIAL PRIMARY KEY);\nCREATE INDEX _clust_multi_idx ON _clust_multi(id);";
    let result = fix_sql(input);

    let cleanup = "DROP INDEX IF EXISTS _clust_multi_idx; DROP TABLE IF EXISTS _clust_multi;";
    let exec_result = cx.exec_file_retry(&result.sql, cleanup);

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
    let cx = ClusterScope::new("clean_types");

    let mut failures = Vec::new();

    for (label, col_type) in common::SUPPORTED_TYPES {
        let tbl = format!("_clust_type_{}", label.replace('-', "_"));
        // Pre-clean in case a previous iteration's cleanup raced or failed.
        let _ = cx.exec(&format!("DROP TABLE IF EXISTS {tbl};"));

        let sql = format!("CREATE TABLE {tbl} (col {col_type});");
        if let Err(err) = cx.exec(&sql) {
            failures.push(format!(
                "[{label}] {col_type}\n  SQL: {sql}\n  Error: {err}"
            ));
        }
        let _ = cx.exec(&format!("DROP TABLE IF EXISTS {tbl};"));
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
    let cx = ClusterScope::new("clean_stmts");
    cx.exec("CREATE TABLE _clean_base (id INT, name TEXT);")
        .expect("base table setup");

    let mut failures = Vec::new();

    for (label, sql, setup_sql, cleanup_sql) in common::CLEAN_STATEMENTS {
        if !setup_sql.is_empty() {
            run_cleanup_stmts(&cx, setup_sql);
        }

        if let Err(err) = cx.exec(sql) {
            failures.push(format!("[{label}] {sql}\n  Error: {err}"));
        }

        if !cleanup_sql.is_empty() {
            run_cleanup_stmts(&cx, cleanup_sql);
        }
    }

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
    let cx = ClusterScope::new("index_variants");
    cx.exec("CREATE TABLE _clust_idxtbl (id INT, name TEXT, val INT);")
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
        let _ = cx.exec(cleanup_sql);
        if let Err(err) = cx.exec(sql) {
            failures.push(format!("[{label}] {sql}\n  Error: {err}"));
        }
        let _ = cx.exec(cleanup_sql);
    }

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
    let cx = ClusterScope::new("seq_variants");

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
        let _ = cx.exec(cleanup_sql);
        if let Err(err) = cx.exec(sql) {
            failures.push(format!("[{label}] {sql}\n  Error: {err}"));
        }
        let _ = cx.exec(cleanup_sql);
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
    let cx = ClusterScope::new("clean_multi");

    let mut failures = Vec::new();

    for (label, sql, cleanup_sql) in common::CLEAN_MULTI_STATEMENT_CASES {
        run_cleanup_stmts(&cx, cleanup_sql);

        if let Err(err) = cx.exec_file_retry(sql, cleanup_sql) {
            failures.push(format!("[{label}]\n  SQL: {sql}\n  Error: {err}"));
        }

        run_cleanup_stmts(&cx, cleanup_sql);
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
//
// Runs sequentially in the test's own schema. The cargo harness parallelizes
// across the 8 cluster #[test] fns, so single-threaded iteration here is
// fine — total wall time is bounded by the slowest test, not the sum.

#[test]
fn lint_rule_fixtures_validated_on_cluster() {
    let cx = ClusterScope::new("rule_fixtures");

    let rules: Vec<LintRule> = LintRule::iter()
        .filter(|r| common::fixture_for_rule(*r).is_some())
        .collect();

    let mut failures = Vec::new();

    // Reset to an empty schema, recreate the shared `_clust_base` table that
    // most fixtures reference, then apply the fixture's own setup SQL. Returns
    // a formatted error string on failure so the caller can attribute it to
    // the right phase ("setup", "fix-path setup", "retry N setup", …).
    let reset_with_setup = |fix: &common::RuleFixture, phase: &str| -> Result<(), String> {
        cx.reset()
            .map_err(|e| format!("{phase} reset failed: {e}"))?;
        cx.exec("CREATE TABLE _clust_base (id INT, col INT);")
            .map_err(|e| format!("{phase} _clust_base setup failed: {e}"))?;
        if !fix.setup_sql.is_empty() {
            cx.exec(fix.setup_sql).map_err(|e| {
                format!(
                    "{phase} setup failed\n  Setup: {}\n  Error: {e}",
                    fix.setup_sql
                )
            })?;
        }
        Ok(())
    };

    for rule in rules {
        let fix = common::fixture_for_rule(rule).expect("filtered above");

        // Assertion 1: linter produces a diagnostic for this rule.
        let diags = lint_sql(fix.sql);
        if !diags.iter().any(|d| d.rule == rule) {
            failures.push(format!(
                "[{rule:?}] fixture does not produce a `{rule:?}` diagnostic\n  Input: {}\n  Got: {diags:?}",
                fix.sql
            ));
            continue;
        }

        // Assertion 2: DSQL rejects the unfixed input.
        if let Err(err) = reset_with_setup(&fix, "reject-path") {
            failures.push(format!("[{rule:?}] {err}"));
            continue;
        }
        let reject_result = if fix.sql.contains(";\n") {
            cx.exec_file_once(fix.sql)
        } else {
            cx.exec(fix.sql)
        };
        if reject_result.is_ok() {
            failures.push(format!(
                "[{rule:?}] expected DSQL to reject unfixed input, but it succeeded. \
                 Check if DSQL now supports this feature — if so, remove the rule. \
                 Otherwise the fixture's setup may have masked the rejection (e.g. the \
                 referenced object exists when it shouldn't, or vice versa).\n  Input: {}",
                fix.sql
            ));
        }

        // Assertion 3: fix_sql's output runs clean on the cluster (when
        // fixable). Filter to the rule under test so an unrelated `Unfixable`
        // diagnostic (e.g. a fixture that incidentally trips another rule)
        // doesn't silently skip fix-path validation for *this* rule.
        let result = fix_sql(fix.sql);
        let has_unfixable = result
            .diagnostics
            .iter()
            .filter(|d| d.rule == rule)
            .any(|d| matches!(d.fix_result, FixResult::Unfixable));
        if has_unfixable || result.sql.is_empty() {
            continue;
        }

        if let Err(err) = reset_with_setup(&fix, "fix-path") {
            failures.push(format!("[{rule:?}] {err}"));
            continue;
        }

        // Multi-statement fix paths can leave partial state when an OC001
        // hits mid-file; reset between retries so each attempt starts clean.
        let mut last_err = None;
        for retry in 0..FIXTURE_FIX_PATH_MAX_RETRIES {
            if retry > 0 {
                if let Err(err) = reset_with_setup(&fix, &format!("retry-{retry}")) {
                    last_err = Some(err);
                    break;
                }
            }
            let attempt = if result.sql.contains(";\n") {
                cx.exec_file_once(&result.sql)
            } else {
                cx.exec(&result.sql)
            };
            match attempt {
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
            failures.push(format!(
                "[{rule:?}]\n  Input: {}\n  Fixed: {}\n  Error: {err}",
                fix.sql, result.sql
            ));
        }
    }

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
    let cx = ClusterScope::new("ddl_txn_fix");

    let mut failures = Vec::new();

    for (label, input_sql, cleanup_sql) in DDL_TXN_FIX_CASES {
        run_cleanup_stmts(&cx, cleanup_sql);

        let result = fix_sql(input_sql);

        if let Err(err) = cx.exec_file_retry(&result.sql, cleanup_sql) {
            failures.push(format!(
                "[{label}]\n  Input:  {input_sql}\n  Fixed:  {}\n  Error:  {err}",
                result.sql
            ));
        }

        run_cleanup_stmts(&cx, cleanup_sql);
    }

    assert!(
        failures.is_empty(),
        "DDL transaction fix failures against DSQL cluster:\n\n{}",
        failures.join("\n\n")
    );
}
