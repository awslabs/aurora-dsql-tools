//! Drift detection: assert dsql-lint and the grammar oracle agree on
//! every known case. New disagreements fail CI; expected disagreements
//! are listed in `EXPECTED_DRIFT` as `(sql, DriftReason)` pairs. Stale
//! entries also fail.
//!
//! `REJECT_SQLS` and `ACCEPT_SQLS` duplicate SQL strings from
//! `tests/integration_test.rs` and `tests/common/mod.rs`. The duplication
//! is intentional: those source arrays carry test-file-local categories
//! and expected-message strings that don't matter to the oracle, and the
//! current `mod common;` layout makes re-export awkward. Keep the lists
//! in sync manually for now; if the duplication becomes painful we can
//! extract a shared module.

use crate::grammar_oracle::pg_corpus;
use crate::grammar_oracle::grammar;
use dsql_lint::lint_sql;
use std::path::Path;
use std::sync::OnceLock;

#[derive(Debug, Clone)]
pub struct Disagreement {
    pub sql: String,
    pub kind: DisagreementKind,
    pub source: CorpusSource,
}

/// Which corpus a disagreement came from. Determines how it's triaged: the
/// hand-curated `dsql-lint` corpus uses per-statement opt-out via
/// `EXPECTED_DRIFT`; the much larger Postgres corpus uses predicate-based
/// skipping (see `should_skip_pg_statement`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CorpusSource {
    /// SQL drawn from `tests/integration_test.rs` and `tests/common/mod.rs` —
    /// what dsql-lint already covers. Curated; every disagreement gets a
    /// hand-tagged `EXPECTED_DRIFT` entry.
    DsqlLint,
    /// Vendored Postgres regression-test SQL (`tests/grammar_oracle/pg_corpus/`).
    /// Bulk; opt-out is by predicate, not per-statement.
    Pg,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DisagreementKind {
    /// dsql-lint flags an error, but the grammar accepts.
    /// Often: grammar relaxed, or the oracle is over-permissive.
    LintFlagsGrammarAccepts,
    /// Grammar rejects, but dsql-lint says clean. Likely: dsql-lint
    /// missing a rule (the common case), or the oracle's input-driven
    /// derivation doesn't yet cover the construct.
    GrammarRejectsLintQuiet,
}

/// Lazily-loaded grammar from `dsql_grammar.json`.
fn grammar() -> &'static grammar::Grammar {
    static G: OnceLock<grammar::Grammar> = OnceLock::new();
    G.get_or_init(|| {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("workspace parent")
            .join("dsql_grammar.json");
        grammar::Grammar::load(&path)
    })
}

fn grammar_accepts(sql: &str) -> bool {
    grammar().accepts(sql)
}

/// SQL strings that should be REJECTED by dsql-lint (i.e. trigger a
/// diagnostic).
///
/// Source of truth: `ERROR_CASES` and `ADDITIONAL_ERROR_CASES` in
/// `tests/integration_test.rs`. Kept in sync manually — see module-level
/// docstring.
pub const REJECT_SQLS: &[&str] = &[
    // -- ERROR_CASES --
    "CREATE TABLE t (id SERIAL PRIMARY KEY);",
    "CREATE TABLE t (id SERIAL4 PRIMARY KEY);",
    "CREATE TABLE t (id BIGSERIAL PRIMARY KEY);",
    "CREATE TABLE t (id SERIAL8 PRIMARY KEY);",
    "CREATE TABLE t (id SMALLSERIAL PRIMARY KEY);",
    "CREATE TABLE t (id SERIAL2 PRIMARY KEY);",
    "CREATE TABLE t (id INT, data JSONB);",
    "CREATE TABLE t (id INT, cid INT REFERENCES c(id));",
    "CREATE TABLE t (id INT, cid INT, FOREIGN KEY (cid) REFERENCES c(id));",
    "CREATE TEMP TABLE t (id INT);",
    "CREATE TEMPORARY TABLE t (id INT);",
    "CREATE TABLE t (id INT, tags TEXT[]);",
    "CREATE TABLE t (id INT, scores INT[]);",
    "CREATE TABLE t (id INT, d DATE) PARTITION BY RANGE (d);",
    "TRUNCATE TABLE orders;",
    "CREATE TRIGGER trg AFTER INSERT ON t FOR EACH ROW EXECUTE FUNCTION f();",
    "CREATE EXTENSION IF NOT EXISTS pgcrypto;",
    "CREATE INDEX idx_foo ON t(col);",
    "CREATE UNIQUE INDEX idx_bar ON t(col);",
    "CREATE INDEX ASYNC idx ON t USING btree(col);",
    "CREATE INDEX ASYNC idx ON t USING gin(col);",
    "CREATE INDEX ASYNC idx ON t USING hash(col);",
    "CREATE INDEX ASYNC idx ON t(col) USING btree;",
    "CREATE INDEX ASYNC idx ON t(col) USING hash;",
    "CREATE INDEX idx ON t(col) USING btree;",
    "CREATE INDEX ASYNC idx ON t (lower(name));",
    "CREATE INDEX CONCURRENTLY idx ON t(col);",
    "CREATE INDEX ASYNC idx ON t(col) WHERE col > 0;",
    "ALTER TABLE t ADD COLUMN id SERIAL;",
    "ALTER TABLE t ADD COLUMN data JSONB;",
    "ALTER TABLE t ADD COLUMN tags TEXT[];",
    "ALTER TABLE t ADD COLUMN cid INT REFERENCES c(id);",
    "ALTER TABLE t ADD CONSTRAINT fk_c FOREIGN KEY (cid) REFERENCES c(id);",
    "CREATE TABLE child (extra INT) INHERITS (parent);",
    "CREATE TABLE t AS SELECT 1 AS id;",
    "CREATE TABLE t (id INT) TABLESPACE my_space;",
    "CREATE TEMPORARY VIEW v AS SELECT 1;",
    "CREATE MATERIALIZED VIEW mv AS SELECT 1;",
    "CREATE DATABASE mydb;",
    "CREATE POLICY p ON t USING (true);",
    "SAVEPOINT sp1;",
    "RELEASE SAVEPOINT sp1;",
    "ROLLBACK TO SAVEPOINT sp1;",
    "DECLARE c CURSOR FOR SELECT 1;",
    "CREATE TYPE mood AS ENUM ('happy', 'sad');",
    "CREATE SERVER s FOREIGN DATA WRAPPER w;",
    "LOCK TABLE t IN ACCESS EXCLUSIVE MODE;",
    "VACUUM;",
    "VACUUM FULL t;",
    "ALTER INDEX idx_name RENAME TO idx_new;",
    "CREATE TABLE t (id INTEGER GENERATED ALWAYS AS IDENTITY);",
    "CREATE TABLE t (id SMALLINT GENERATED BY DEFAULT AS IDENTITY);",
    "CREATE SEQUENCE s AS INTEGER;",
    "CREATE SEQUENCE s CACHE 100;",
    "CREATE SEQUENCE s CACHE 2;",
    "CREATE SEQUENCE s CACHE 65535;",
    "CREATE SEQUENCE s CACHE -1;",
    "CREATE SEQUENCE s CACHE 0;",
    "CREATE TABLE t (id BIGINT GENERATED ALWAYS AS IDENTITY (CACHE 100));",
    "ALTER TABLE t ENABLE ROW LEVEL SECURITY;",
    "ALTER TABLE t DISABLE ROW LEVEL SECURITY;",
    "ALTER TABLE t FORCE ROW LEVEL SECURITY;",
    "ALTER TABLE t NO FORCE ROW LEVEL SECURITY;",
    "ALTER TABLE t ENABLE TRIGGER trg1;",
    "ALTER TABLE t DISABLE TRIGGER trg1;",
    "ALTER TABLE t ENABLE ALWAYS TRIGGER trg1;",
    "ALTER TABLE t ENABLE REPLICA TRIGGER trg1;",
    "ALTER TABLE t REPLICA IDENTITY FULL;",
    "ALTER TABLE t VALIDATE CONSTRAINT c1;",
    "CREATE INDEX ASYNC idx ON t (col, lower(name));",
    "CREATE INDEX ASYNC idx ON t(a.b);",
    "ALTER TABLE t ENABLE RULE r1;",
    "ALTER TABLE t DISABLE RULE r1;",
    "ALTER TABLE t ENABLE ALWAYS RULE r1;",
    "ALTER TABLE t ENABLE REPLICA RULE r1;",
    "ALTER TABLE t ADD PRIMARY KEY USING INDEX my_idx;",
    "ALTER TABLE t ADD UNIQUE USING INDEX my_idx;",
    "COPY t FROM '/tmp/data.csv';",
    "COPY t TO '/tmp/data.csv';",
    "COPY t FROM PROGRAM 'cat /tmp/data.csv';",
    // -- ADDITIONAL_ERROR_CASES --
    "ALTER TABLE t ADD COLUMN status VARCHAR(50) DEFAULT 'pending';",
    "ALTER TABLE t ADD COLUMN status VARCHAR(50) NOT NULL;",
    "BEGIN ISOLATION LEVEL SERIALIZABLE;",
    "BEGIN ISOLATION LEVEL READ COMMITTED;",
    "SET TRANSACTION ISOLATION LEVEL SERIALIZABLE;",
    "CREATE SEQUENCE s;",
    "CREATE SEQUENCE s INCREMENT 1;",
    "ALTER TABLE t DROP COLUMN name;",
    "ALTER TABLE t ALTER COLUMN name TYPE TEXT;",
    "ALTER TABLE t ALTER COLUMN name SET NOT NULL;",
    "ALTER TABLE t ALTER COLUMN name DROP NOT NULL;",
    "ALTER TABLE t ALTER COLUMN name SET DEFAULT 'foo';",
    "ALTER TABLE t ALTER COLUMN name DROP DEFAULT;",
    "ALTER TABLE t ADD CONSTRAINT c CHECK (id > 0);",
    "ALTER TABLE t ADD CONSTRAINT c UNIQUE (name);",
    "ALTER TABLE t DROP CONSTRAINT c;",
    "CREATE TABLE t (id BIGINT GENERATED ALWAYS AS IDENTITY);",
    "CREATE TABLE t (id BIGINT GENERATED BY DEFAULT AS IDENTITY);",
    "ALTER TABLE t ADD COLUMN id BIGINT GENERATED ALWAYS AS IDENTITY;",
    "ALTER TABLE t ADD COLUMN id BIGINT GENERATED BY DEFAULT AS IDENTITY;",
    "ALTER TABLE t ALTER COLUMN id ADD GENERATED ALWAYS AS IDENTITY;",
    "ALTER TABLE t ALTER COLUMN id ADD GENERATED BY DEFAULT AS IDENTITY;",
    "ALTER TABLE t ALTER COLUMN id ADD GENERATED ALWAYS AS IDENTITY (CACHE 1);",
];

/// SQL strings that should lint clean (no diagnostics).
///
/// Source of truth: `FALSE_POSITIVE_CASES` and `ADDITIONAL_FALSE_POSITIVES`
/// SQLs in `tests/integration_test.rs` and `CLEAN_STATEMENTS` in
/// `tests/common/mod.rs`, plus `SUPPORTED_TYPES` (each wrapped in a
/// `CREATE TABLE`). Kept in sync manually — see module-level docstring.
///
/// Note: `FALSE_POSITIVE_CASES` originally tracks "must NOT contain
/// substring X"; here we use the same SQLs as proxies for "lints clean
/// overall". A handful of those SQLs intentionally lint with *other*
/// errors (e.g. `CREATE INDEX CONCURRENTLY idx ON t(col);` triggers the
/// CONCURRENTLY rule); we exclude those and rely on the integration tests
/// to enforce the per-substring assertions.
pub const ACCEPT_SQLS: &[&str] = &[
    // -- FALSE_POSITIVE_CASES (subset that lints fully clean) --
    "CREATE TABLE t (id UUID PRIMARY KEY, serial_number VARCHAR(100));",
    "CREATE TABLE t (id UUID PRIMARY KEY, json_data TEXT);",
    "CREATE TABLE t (id UUID PRIMARY KEY, data JSON);",
    "CREATE TABLE temporary_cache (id UUID PRIMARY KEY);",
    "CREATE TABLE t (id UUID PRIMARY KEY, tags TEXT);",
    "CREATE TABLE t (id UUID PRIMARY KEY, partition_key VARCHAR(50));",
    "DELETE FROM events WHERE id = 1;",
    "CREATE TABLE t (id BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY);",
    "CREATE TABLE t (id BIGINT GENERATED BY DEFAULT AS IDENTITY PRIMARY KEY);",
    "CREATE TABLE t (id UUID PRIMARY KEY, inherits_from TEXT);",
    "CREATE INDEX ASYNC idx ON t(col);",
    "CREATE VIEW v AS SELECT 1;",
    "CREATE TABLE t (id BIGINT GENERATED ALWAYS AS IDENTITY (CACHE 1));",
    "CREATE TABLE t (id BIGINT GENERATED BY DEFAULT AS IDENTITY (CACHE 1));",
    "CREATE SEQUENCE s AS BIGINT CACHE 65536;",
    "CREATE SEQUENCE s CACHE 1;",
    "CREATE SEQUENCE s CACHE 65536;",
    "CREATE TABLE t (id BIGINT GENERATED ALWAYS AS IDENTITY (CACHE 65536));",
    "CREATE SEQUENCE s CACHE 65537;",
    "ROLLBACK;",
    "ALTER TABLE t ADD COLUMN x TEXT;",
    "ALTER TABLE t OWNER TO new_owner;",
    "COPY t FROM STDIN;",
    "COPY t TO STDOUT;",
    // -- ADDITIONAL_FALSE_POSITIVES (entries not already covered above) --
    "ALTER TABLE t ADD COLUMN status VARCHAR(50);",
    "ALTER TABLE t ADD COLUMN name TEXT;",
    "ALTER TABLE t RENAME COLUMN a TO b;",
    "ALTER TABLE t ADD COLUMN id BIGINT GENERATED ALWAYS AS IDENTITY (CACHE 1);",
    // -- CLEAN_STATEMENTS --
    "CREATE VIEW _clean_view AS SELECT 1;",
    "ALTER TABLE _clean_base ADD COLUMN description TEXT;",
    "INSERT INTO _clean_base (id, name) VALUES (1, 'test');",
    "SELECT * FROM _clean_base WHERE id = 1;",
    "UPDATE _clean_base SET name = 'updated' WHERE id = 1;",
    "DELETE FROM _clean_base WHERE id = 1;",
    "BEGIN ISOLATION LEVEL REPEATABLE READ;",
    "BEGIN;",
    "CREATE VIEW _clean_view2 AS SELECT 1;",
    "INSERT INTO _clean_base (id, name) VALUES (2, 'TRUNCATE TABLE foo; CREATE TRIGGER bar');",
    // -- SUPPORTED_TYPES wrapped in CREATE TABLE --
    "CREATE TABLE _type_test (col SMALLINT);",
    "CREATE TABLE _type_test (col INT2);",
    "CREATE TABLE _type_test (col INTEGER);",
    "CREATE TABLE _type_test (col INT);",
    "CREATE TABLE _type_test (col INT4);",
    "CREATE TABLE _type_test (col BIGINT);",
    "CREATE TABLE _type_test (col INT8);",
    "CREATE TABLE _type_test (col REAL);",
    "CREATE TABLE _type_test (col FLOAT4);",
    "CREATE TABLE _type_test (col DOUBLE PRECISION);",
    "CREATE TABLE _type_test (col FLOAT8);",
    "CREATE TABLE _type_test (col NUMERIC(10,2));",
    "CREATE TABLE _type_test (col DECIMAL(18,6));",
    "CREATE TABLE _type_test (col DEC(5,2));",
    "CREATE TABLE _type_test (col CHAR(10));",
    "CREATE TABLE _type_test (col CHARACTER(20));",
    "CREATE TABLE _type_test (col VARCHAR(255));",
    "CREATE TABLE _type_test (col CHARACTER VARYING(100));",
    "CREATE TABLE _type_test (col TEXT);",
    "CREATE TABLE _type_test (col BPCHAR(50));",
    "CREATE TABLE _type_test (col DATE);",
    "CREATE TABLE _type_test (col TIME);",
    "CREATE TABLE _type_test (col TIME WITH TIME ZONE);",
    "CREATE TABLE _type_test (col TIMESTAMP);",
    "CREATE TABLE _type_test (col TIMESTAMP WITH TIME ZONE);",
    "CREATE TABLE _type_test (col INTERVAL);",
    "CREATE TABLE _type_test (col BOOLEAN);",
    "CREATE TABLE _type_test (col BOOL);",
    "CREATE TABLE _type_test (col BYTEA);",
    "CREATE TABLE _type_test (col UUID);",
    "CREATE TABLE _type_test (col BIGINT GENERATED ALWAYS AS IDENTITY (CACHE 1));",
    "CREATE TABLE _type_test (col BIGINT GENERATED BY DEFAULT AS IDENTITY (CACHE 1));",
    "CREATE TABLE _type_test (col BIGINT GENERATED ALWAYS AS IDENTITY (CACHE 65536));",
    "CREATE TABLE _type_test (col BIGINT GENERATED BY DEFAULT AS IDENTITY (CACHE 65536));",
];

pub fn collect() -> Vec<Disagreement> {
    let mut out = Vec::new();

    // dsql-lint corpus: every disagreement is meaningful (curated).
    for sql in REJECT_SQLS.iter().chain(ACCEPT_SQLS.iter()) {
        if let Some(kind) = classify(sql) {
            out.push(Disagreement {
                sql: (*sql).to_string(),
                kind,
                source: CorpusSource::DsqlLint,
            });
        }
    }

    // Postgres corpus: predicate-skip disagreements that aren't dsql-lint
    // signal (parse errors). What's left is the actionable burndown.
    for sql in pg_corpus::statements() {
        if should_skip_pg_statement(sql) {
            continue;
        }
        if let Some(kind) = classify(sql) {
            out.push(Disagreement {
                sql: sql.clone(),
                kind,
                source: CorpusSource::Pg,
            });
        }
    }

    out
}

/// Classify a single statement against dsql-lint and the grammar oracle.
/// Returns `None` if they agree, or the kind of disagreement otherwise.
fn classify(sql: &str) -> Option<DisagreementKind> {
    let lint_flags = !lint_sql(sql).is_empty();
    let accepts = grammar_accepts(sql);
    match (lint_flags, accepts) {
        (true, true) => Some(DisagreementKind::LintFlagsGrammarAccepts),
        (false, false) => Some(DisagreementKind::GrammarRejectsLintQuiet),
        _ => None,
    }
}

/// Statements `lint_sql` flags as `ParseError` get skipped: dsql-lint
/// can't lint what it can't parse, so they aren't drift signal. The PG
/// corpus contains intentionally broken SQL marked `-- fail` plus exotic
/// constructs `sqlparser-dsql` doesn't model — both surface as ParseError.
fn should_skip_pg_statement(sql: &str) -> bool {
    has_parse_error(sql)
}

pub fn has_parse_error(sql: &str) -> bool {
    use dsql_lint::LintRule;
    lint_sql(sql).iter().any(|d| d.rule == LintRule::ParseError)
}

/// Why a known disagreement is tolerated. Tagging makes the burndown list
/// actionable: a maintainer looking for "what rule should we add next?"
/// just filters by [`DriftReason::MissingDsqlLintRule`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)] // not all variants are populated today; they're part of the documented surface
pub enum DriftReason {
    /// Grammar permits-by-design; dsql-lint's check is semantic. The grammar
    /// can't express the restriction without a parser/sema split it doesn't
    /// have. Won't shrink — these stay tolerated unless DSQL itself relaxes.
    SemanticOnly,
    /// Grammar correctly rejects; dsql-lint is silent. Adding a rule to
    /// dsql-lint removes the entry. **This is the actionable burndown list.**
    MissingDsqlLintRule,
    /// Oracle's input-driven derivation can't follow this grammar shape.
    /// Sub-categories represented in `EXPECTED_DRIFT` today: undefined-
    /// nonterminal stub (`SignedIconst`), `*Stmt` rules absent from the
    /// synthesized start set (`CopyStmt`), deep `a_expr` paths, and
    /// transaction/alter-table branches the engine doesn't fully expand
    /// (`BEGIN;`, `ALTER TABLE … OWNER TO`). The grammar would give the
    /// right answer if the oracle faithfully matched it — oracle work,
    /// not rule work.
    RecognizerHole,
}

/// Cases from the curated dsql-lint corpus where dsql-lint and the
/// grammar disagree, tolerated for now. Each entry is `(sql, reason)`.
/// The test fails on stale entries, so the list naturally shrinks.
///
/// To find the next dsql-lint rule to write, filter entries whose reason
/// is [`DriftReason::MissingDsqlLintRule`].
pub const EXPECTED_DRIFT: &[(&str, DriftReason)] = &[
    // -- SemanticOnly --
    // The grammar's `Typename → GenericType → type_function_name →
    // identifier` path matches any identifier as a type name (Postgres
    // allows user-defined types to look the same as built-ins). dsql-lint
    // flags `SERIAL`, `JSONB`, etc. at the semantic layer; the grammar
    // permits the surface syntax.
    (
        "CREATE TABLE t (id SERIAL PRIMARY KEY);",
        DriftReason::SemanticOnly,
    ),
    (
        "CREATE TABLE t (id SERIAL4 PRIMARY KEY);",
        DriftReason::SemanticOnly,
    ),
    (
        "CREATE TABLE t (id BIGSERIAL PRIMARY KEY);",
        DriftReason::SemanticOnly,
    ),
    (
        "CREATE TABLE t (id SERIAL8 PRIMARY KEY);",
        DriftReason::SemanticOnly,
    ),
    (
        "CREATE TABLE t (id SMALLSERIAL PRIMARY KEY);",
        DriftReason::SemanticOnly,
    ),
    (
        "CREATE TABLE t (id SERIAL2 PRIMARY KEY);",
        DriftReason::SemanticOnly,
    ),
    (
        "CREATE TABLE t (id INT, data JSONB);",
        DriftReason::SemanticOnly,
    ),
    (
        "ALTER TABLE t ADD COLUMN id SERIAL;",
        DriftReason::SemanticOnly,
    ),
    (
        "ALTER TABLE t ADD COLUMN data JSONB;",
        DriftReason::SemanticOnly,
    ),
    // Array type column (`TYPE[]`) is allowed by the grammar's Typename;
    // dsql-lint flags arrays semantically.
    (
        "CREATE TABLE t (id INT, tags TEXT[]);",
        DriftReason::SemanticOnly,
    ),
    (
        "CREATE TABLE t (id INT, scores INT[]);",
        DriftReason::SemanticOnly,
    ),
    (
        "ALTER TABLE t ADD COLUMN tags TEXT[];",
        DriftReason::SemanticOnly,
    ),
    // ADD COLUMN with NOT NULL: grammar accepts the column-constraint
    // grammar shape; dsql-lint has the AddColumnConstraint rule.
    (
        "ALTER TABLE t ADD COLUMN status VARCHAR(50) NOT NULL;",
        DriftReason::SemanticOnly,
    ),
    // Index expressions: the grammar's IndexElem accepts function-call
    // expressions; dsql-lint has the IndexExpression rule.
    (
        "CREATE INDEX ASYNC idx ON t (lower(name));",
        DriftReason::SemanticOnly,
    ),
    (
        "CREATE INDEX ASYNC idx ON t (col, lower(name));",
        DriftReason::SemanticOnly,
    ),
    // Partial index: grammar's IndexStmt has `where_clause`; dsql-lint
    // has the IndexPartial rule.
    (
        "CREATE INDEX ASYNC idx ON t(col) WHERE col > 0;",
        DriftReason::SemanticOnly,
    ),
    // CHECK constraint: grammar accepts; dsql-lint has the
    // AddColumnConstraint rule for ADD CONSTRAINT.
    (
        "ALTER TABLE t ADD CONSTRAINT c CHECK (id > 0);",
        DriftReason::SemanticOnly,
    ),
    // -- RecognizerHole --
    // CACHE clauses on identity columns / sequences route through the
    // grammar's `SignedIconst` rule, which is undefined in the JSON.
    // Our oracle stubs undefined nonterminals as "consume one token" but
    // the actual grammar shape is `[+/-]? ICONST`, and our stub doesn't
    // match the pattern needed in this position.
    (
        "CREATE TABLE t (id BIGINT GENERATED ALWAYS AS IDENTITY (CACHE 1));",
        DriftReason::RecognizerHole,
    ),
    (
        "CREATE TABLE t (id BIGINT GENERATED ALWAYS AS IDENTITY (CACHE 65536));",
        DriftReason::RecognizerHole,
    ),
    (
        "CREATE TABLE _type_test (col BIGINT GENERATED ALWAYS AS IDENTITY (CACHE 1));",
        DriftReason::RecognizerHole,
    ),
    (
        "CREATE TABLE _type_test (col BIGINT GENERATED ALWAYS AS IDENTITY (CACHE 65536));",
        DriftReason::RecognizerHole,
    ),
    ("CREATE SEQUENCE s CACHE 1;", DriftReason::RecognizerHole),
    (
        "CREATE SEQUENCE s CACHE 65536;",
        DriftReason::RecognizerHole,
    ),
    // ALTER INDEX RENAME and ALTER TABLE … DROP DEFAULT: grammar accepts
    // both shapes; dsql-lint flags via UnsupportedAlterTableOp.
    (
        "ALTER INDEX idx_name RENAME TO idx_new;",
        DriftReason::SemanticOnly,
    ),
    (
        "ALTER TABLE t ALTER COLUMN name DROP DEFAULT;",
        DriftReason::SemanticOnly,
    ),
    // GENERATED BY DEFAULT IDENTITY (CACHE …): SignedIconst stub.
    (
        "CREATE TABLE t (id BIGINT GENERATED BY DEFAULT AS IDENTITY (CACHE 1));",
        DriftReason::RecognizerHole,
    ),
    (
        "CREATE TABLE _type_test (col BIGINT GENERATED BY DEFAULT AS IDENTITY (CACHE 1));",
        DriftReason::RecognizerHole,
    ),
    (
        "CREATE TABLE _type_test (col BIGINT GENERATED BY DEFAULT AS IDENTITY (CACHE 65536));",
        DriftReason::RecognizerHole,
    ),
    // CREATE SEQUENCE … CACHE: same SignedIconst stub.
    (
        "CREATE SEQUENCE s AS BIGINT CACHE 65536;",
        DriftReason::RecognizerHole,
    ),
    (
        "CREATE SEQUENCE s CACHE 65537;",
        DriftReason::RecognizerHole,
    ),
    // BEGIN forms: the grammar's TransactionStmt enumerates ABORT/
    // START/COMMIT/ROLLBACK but not bare BEGIN; oracle can't follow.
    ("BEGIN;", DriftReason::RecognizerHole),
    (
        "BEGIN ISOLATION LEVEL REPEATABLE READ;",
        DriftReason::RecognizerHole,
    ),
    // ALTER TABLE OWNER TO: oracle's alter_table_cmds derivation doesn't
    // follow through to the relevant grammar branch.
    (
        "ALTER TABLE t OWNER TO new_owner;",
        DriftReason::RecognizerHole,
    ),
    // COPY: not modeled in the start-rule synthesis (statement_rules()
    // returns only `*Stmt` rules; CopyStmt isn't one of them).
    ("COPY t FROM STDIN;", DriftReason::RecognizerHole),
    ("COPY t TO STDOUT;", DriftReason::RecognizerHole),
    // SELECT/INSERT/UPDATE/DELETE: dsql-lint accepts these; the grammar
    // should accept them too. The oracle currently rejects them because
    // the recursive a_expr / SELECT / WHERE clause expansions aren't fully
    // followed by the input-driven derivation. Tagged as oracle gaps
    // until the engine covers them.
    (
        "DELETE FROM events WHERE id = 1;",
        DriftReason::RecognizerHole,
    ),
    (
        "DELETE FROM _clean_base WHERE id = 1;",
        DriftReason::RecognizerHole,
    ),
    (
        "UPDATE _clean_base SET name = 'updated' WHERE id = 1;",
        DriftReason::RecognizerHole,
    ),
    // -- MissingDsqlLintRule --
    // (initially empty — no entries in this category yet from the curated
    //  corpus. PG-corpus disagreements are the larger surface; see the
    //  pg_corpus_drift_report test.)
];
