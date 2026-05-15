//! Drift detection: assert dsql-lint and the recognizer agree on every
//! known case. New disagreements fail CI; expected disagreements are
//! listed in `EXPECTED_DRIFT` as `(sql, DriftReason)` pairs. Stale
//! entries also fail.
//!
//! `REJECT_SQLS` and `ACCEPT_SQLS` duplicate SQL strings from
//! `tests/integration_test.rs` and `tests/common/mod.rs`. The duplication
//! is intentional: those source arrays carry test-file-local categories
//! and expected-message strings that don't matter to the oracle, and the
//! current `mod common;` layout makes re-export awkward. Keep the lists
//! in sync manually for now; if the duplication becomes painful we can
//! extract a shared module.

use crate::grammar_oracle::ebnf::{parse_grammar, Grammar};
use crate::grammar_oracle::recognizer::Recognizer;
use dsql_lint::lint_sql;
use std::path::Path;
use std::sync::OnceLock;

#[derive(Debug, Clone)]
pub struct Disagreement {
    pub sql: String,
    pub kind: DisagreementKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DisagreementKind {
    /// dsql-lint flags an error, but the grammar accepts.
    /// Often: grammar relaxed, or recognizer is over-permissive.
    LintFlagsGrammarAccepts,
    /// Grammar rejects, but dsql-lint says clean. Likely: dsql-lint
    /// missing a rule (the common case), or the recognizer's expression
    /// subset doesn't cover the construct.
    GrammarRejectsLintQuiet,
}

/// Top-level statement productions in the EBNF. The grammar has no single
/// statement-level start; we tried building a synthetic union production
/// (`Stmt1 | Stmt2 | …`) and it caused chumsky's backtracking to blow up
/// (50+ GB resident, hours of CPU on ~140 inputs). Instead we *dispatch*
/// from the SQL's leading keyword to a single statement-shaped production,
/// then run the recognizer with that as the start. This avoids the 32-way
/// `or`-chain at parse time entirely.
///
/// Each `(keyword_pattern, start_production)` row matches an input where
/// the upper-cased first token (or first two tokens, joined by space)
/// equals `keyword_pattern`. Two-token patterns are checked first so that
/// e.g. `CREATE INDEX` wins over `CREATE`.
///
/// SQL whose leading keyword isn't covered here falls through to a
/// reject-all behavior (the recognizer says "no match"); those statements
/// land in `EXPECTED_DRIFT` if dsql-lint considers them clean. That's
/// fine — the dispatch table is meant to be the simplest thing that
/// works for the corpus we have today, not a complete cover.
#[allow(clippy::type_complexity)]
// INVARIANT: two-token entries must precede single-token entries with the
// same first token. `dispatch_start` returns the first match, not the
// longest — adding e.g. `(&["CREATE"], …)` above the two-token CREATE
// rows would silently shadow them.
const STMT_DISPATCH: &[(&[&str], &str)] = &[
    // Two-token leads (checked first).
    (&["CREATE", "TABLE"], "CreateStmt"),
    (&["CREATE", "TEMP"], "CreateStmt"),
    (&["CREATE", "TEMPORARY"], "CreateStmt"), // also covers TEMPORARY VIEW
    (&["CREATE", "UNIQUE"], "IndexStmt"),
    (&["CREATE", "INDEX"], "IndexStmt"),
    (&["CREATE", "VIEW"], "ViewStmt"),
    (&["CREATE", "MATERIALIZED"], "ViewStmt"),
    (&["CREATE", "SEQUENCE"], "CreateSeqStmt"),
    (&["CREATE", "SCHEMA"], "CreateSchemaStmt"),
    (&["CREATE", "ROLE"], "CreateRoleStmt"),
    (&["CREATE", "DOMAIN"], "CreateDomainStmt"),
    (&["ALTER", "TABLE"], "AlterTableStmt"),
    (&["ALTER", "SEQUENCE"], "AlterSeqStmt"),
    (&["ALTER", "DOMAIN"], "AlterDomainStmt"),
    (&["ALTER", "ROLE"], "AlterRoleStmt"),
    (&["ALTER", "INDEX"], "RenameStmt"), // RenameStmt is a multi-shape rename grammar
    (&["ALTER", "FUNCTION"], "RenameStmt"),
    (&["DROP", "ROLE"], "DropRoleStmt"),
    (&["DROP", "USER"], "DropRoleStmt"),
    (&["DROP", "GROUP"], "DropRoleStmt"),
    (&["SET", "TRANSACTION"], "VariableSetStmt"),
    (&["SET", "LOCAL"], "VariableSetStmt"),
    (&["SET", "SESSION"], "VariableSetStmt"),
    (&["DEALLOCATE", "ALL"], "DeallocateStmt"),
    (&["DEALLOCATE", "PREPARE"], "DeallocateStmt"),
    // Single-token leads.
    (&["SELECT"], "SelectStmt"),
    (&["WITH"], "SelectStmt"), // CTE — SelectStmt path
    (&["INSERT"], "InsertStmt"),
    (&["UPDATE"], "UpdateStmt"),
    (&["DELETE"], "DeleteStmt"),
    (&["EXPLAIN"], "ExplainStmt"),
    (&["COMMENT"], "CommentStmt"),
    (&["GRANT"], "GrantStmt"),
    (&["REVOKE"], "RevokeStmt"),
    (&["DROP"], "DropStmt"),
    (&["SET"], "VariableSetStmt"),
    (&["RESET"], "VariableResetStmt"),
    (&["SHOW"], "VariableShowStmt"),
    (&["BEGIN"], "TransactionStmt"),
    (&["START"], "TransactionStmt"),
    (&["COMMIT"], "TransactionStmt"),
    (&["ROLLBACK"], "TransactionStmt"),
    (&["ABORT"], "TransactionStmt"),
    (&["END"], "TransactionStmt"),
];

/// Lazy collection of one `Recognizer` per distinct start production used
/// in `STMT_DISPATCH`. Each recognizer holds its own clone of the grammar.
fn recognizer_for(start: &str) -> &'static Recognizer {
    use std::collections::HashMap;
    use std::sync::Mutex;
    static MAP: OnceLock<Mutex<HashMap<String, &'static Recognizer>>> = OnceLock::new();
    let map = MAP.get_or_init(|| Mutex::new(HashMap::new()));
    let mut guard = map.lock().expect("recognizer cache mutex poisoned");
    if let Some(r) = guard.get(start) {
        return r;
    }
    let g = grammar().clone();
    let r: &'static Recognizer = Box::leak(Box::new(Recognizer::build(g, start)));
    guard.insert(start.to_string(), r);
    r
}

fn grammar() -> &'static Grammar {
    static G: OnceLock<Grammar> = OnceLock::new();
    G.get_or_init(|| {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("workspace parent")
            .join("dsql_grammar.ebnf");
        let text = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
        parse_grammar(&text).expect("grammar parse")
    })
}

/// Pick the start production for `sql` based on its leading keyword(s).
/// Returns `None` if no dispatch entry matches — the caller treats that
/// as "recognizer rejects".
fn dispatch_start(sql: &str) -> Option<&'static str> {
    let upper = sql.trim().to_ascii_uppercase();
    let tokens: Vec<&str> = upper.split_whitespace().take(2).collect();
    for (pattern, start) in STMT_DISPATCH {
        if pattern.len() <= tokens.len() && pattern.iter().zip(tokens.iter()).all(|(p, t)| p == t) {
            return Some(*start);
        }
    }
    None
}

/// Start productions whose chumsky recognizer parse exhibits exponential
/// backtracking on real-world SQL — running it OOMs the test process.
/// We hard-skip these (treat as "recognizer rejects"), which means clean
/// DML/SELECT inputs land in `EXPECTED_DRIFT` as `GrammarRejectsLintQuiet`.
/// That's the same outcome we'd get from a real recognizer run that
/// happened to reject; the only loss is that we can't actually validate
/// agreement on these statement shapes.
const RECOGNIZER_BLOCKLIST: &[&str] = &[
    "SelectStmt",
    "InsertStmt",
    "UpdateStmt",
    "DeleteStmt",
    "ExplainStmt",
];

fn grammar_accepts(sql: &str) -> bool {
    let stripped = strip_trailing_semi(sql);
    match dispatch_start(stripped) {
        Some(start) if RECOGNIZER_BLOCKLIST.contains(&start) => false,
        Some(start) => recognizer_for(start).accepts(stripped),
        None => false,
    }
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
/// Source of truth: `FALSE_POSITIVE_CASES` SQLs in
/// `tests/integration_test.rs` and `CLEAN_STATEMENTS` in
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
    for sql in REJECT_SQLS.iter().chain(ACCEPT_SQLS.iter()) {
        let lint_flags = !lint_sql(sql).is_empty();
        let accepts = grammar_accepts(sql);
        match (lint_flags, accepts) {
            (true, true) => out.push(Disagreement {
                sql: (*sql).to_string(),
                kind: DisagreementKind::LintFlagsGrammarAccepts,
            }),
            (false, false) => out.push(Disagreement {
                sql: (*sql).to_string(),
                kind: DisagreementKind::GrammarRejectsLintQuiet,
            }),
            _ => {}
        }
    }
    out
}

fn strip_trailing_semi(s: &str) -> &str {
    s.trim().trim_end_matches(';').trim_end()
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
    /// Recognizer is incomplete (production not modelled, blocklisted
    /// statement, lexer stub). The grammar would give the right answer if
    /// our recognizer faithfully matched it. Recognizer work, not rule work.
    RecognizerHole,
}

/// Cases where dsql-lint and the grammar disagree, tolerated for now.
/// Each entry is `(sql, reason)`. The test fails on stale entries, so the
/// list naturally shrinks.
///
/// To find the next dsql-lint rule to write, filter entries whose reason
/// is [`DriftReason::MissingDsqlLintRule`].
pub const EXPECTED_DRIFT: &[(&str, DriftReason)] = &[
    // -- SemanticOnly --
    // The EBNF lets `GenericType -> type_function_name -> identifier` match
    // any identifier as a type name, so SERIAL/JSONB/etc. parse cleanly.
    // dsql-lint flags them at the semantic layer.
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
    // Array type column (`TYPE[]`) is allowed by the EBNF's Typename grammar.
    (
        "CREATE TABLE t (id INT, tags TEXT[]);",
        DriftReason::SemanticOnly,
    ),
    (
        "CREATE TABLE t (id INT, scores INT[]);",
        DriftReason::SemanticOnly,
    ),
    // The EBNF's IndexStmt has `where_clause` so partial indexes parse.
    (
        "CREATE INDEX ASYNC idx ON t(col) WHERE col > 0;",
        DriftReason::SemanticOnly,
    ),
    // RenameStmt is broad; dsql-lint flags this specific shape.
    (
        "ALTER INDEX idx_name RENAME TO idx_new;",
        DriftReason::SemanticOnly,
    ),
    // alter_table_cmds covers these; dsql-lint flags by op semantics.
    (
        "ALTER TABLE t ALTER COLUMN name DROP DEFAULT;",
        DriftReason::SemanticOnly,
    ),
    (
        "ALTER TABLE t ADD CONSTRAINT c CHECK (id > 0);",
        DriftReason::SemanticOnly,
    ),
    // -- RecognizerHole --
    // SELECT/INSERT/UPDATE/DELETE/EXPLAIN are on the recognizer blocklist
    // (chumsky backtracks pathologically on the EBNF's SelectStmt).
    (
        "DELETE FROM events WHERE id = 1;",
        DriftReason::RecognizerHole,
    ),
    (
        "INSERT INTO _clean_base (id, name) VALUES (1, 'test');",
        DriftReason::RecognizerHole,
    ),
    (
        "SELECT * FROM _clean_base WHERE id = 1;",
        DriftReason::RecognizerHole,
    ),
    (
        "UPDATE _clean_base SET name = 'updated' WHERE id = 1;",
        DriftReason::RecognizerHole,
    ),
    (
        "DELETE FROM _clean_base WHERE id = 1;",
        DriftReason::RecognizerHole,
    ),
    (
        "INSERT INTO _clean_base (id, name) VALUES (2, 'TRUNCATE TABLE foo; CREATE TRIGGER bar');",
        DriftReason::RecognizerHole,
    ),
    // `parameter`, `SignedIconst`, `var_list` are reject-everything stubs in
    // `parser_for_undefined`; identity/sequence/CACHE inputs route through them.
    (
        "CREATE TABLE t (id BIGINT GENERATED BY DEFAULT AS IDENTITY (CACHE 1));",
        DriftReason::RecognizerHole,
    ),
    (
        "CREATE SEQUENCE s AS BIGINT CACHE 65536;",
        DriftReason::RecognizerHole,
    ),
    (
        "CREATE SEQUENCE s CACHE 65537;",
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
    // Plain `BEGIN` and `BEGIN ISOLATION LEVEL …` aren't in the EBNF's
    // TransactionStmt (which only lists ABORT/START/COMMIT/ROLLBACK).
    (
        "BEGIN ISOLATION LEVEL REPEATABLE READ;",
        DriftReason::RecognizerHole,
    ),
    ("BEGIN;", DriftReason::RecognizerHole),
    // ALTER TABLE ADD COLUMN bare type — the EBNF's ColumnDef path doesn't
    // accept this through our current recursion.
    (
        "ALTER TABLE _clean_base ADD COLUMN description TEXT;",
        DriftReason::RecognizerHole,
    ),
    (
        "ALTER TABLE t ADD COLUMN x TEXT;",
        DriftReason::RecognizerHole,
    ),
    // ALTER TABLE OWNER TO is not in the alter_table_cmds we expand.
    (
        "ALTER TABLE t OWNER TO new_owner;",
        DriftReason::RecognizerHole,
    ),
    // COPY isn't covered by any Stmt our dispatch table maps.
    ("COPY t FROM STDIN;", DriftReason::RecognizerHole),
    ("COPY t TO STDOUT;", DriftReason::RecognizerHole),
    // Multi-word type names: the EBNF expresses these as fixed sequences
    // we don't currently match through the Typename → identifier path.
    (
        "CREATE TABLE _type_test (col DOUBLE PRECISION);",
        DriftReason::RecognizerHole,
    ),
    (
        "CREATE TABLE _type_test (col CHARACTER VARYING(100));",
        DriftReason::RecognizerHole,
    ),
    (
        "CREATE TABLE _type_test (col TIME WITH TIME ZONE);",
        DriftReason::RecognizerHole,
    ),
    (
        "CREATE TABLE _type_test (col TIMESTAMP WITH TIME ZONE);",
        DriftReason::RecognizerHole,
    ),
    // -- MissingDsqlLintRule --
    // (initially empty — no entries in this category yet. Triage future
    //  drift here when the grammar correctly rejects something dsql-lint
    //  should also flag.)
];
