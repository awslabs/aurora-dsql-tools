#![cfg(feature = "grammar-diff")]

use dsql_lint::grammar::tokenize::PRODUCED_CHARCLASSES;
use dsql_lint::grammar::Grammar;
use std::path::PathBuf;

fn grammar_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("grammar/dsql_grammar.json")
}

/// Catches a grammar refresh that introduces a new CharClass we haven't
/// taught the tokenizer to emit — the silent-noise failure mode this tool
/// is most exposed to.
#[test]
fn every_charclass_in_grammar_has_a_producer() {
    let g = Grammar::load(&grammar_path()).expect("load grammar");
    let referenced = g.referenced_charclasses();
    let produced: std::collections::BTreeSet<&str> = PRODUCED_CHARCLASSES.iter().copied().collect();

    let missing: Vec<&String> = referenced
        .iter()
        .filter(|c| !produced.contains(c.as_str()))
        .collect();
    assert!(
        missing.is_empty(),
        "Grammar references CharClass(es) the tokenizer doesn't produce: {missing:?}. \
         Teach `tokenize::map_token` to emit each and add the name to \
         `PRODUCED_CHARCLASSES`.",
    );
}

fn case(g: &Grammar, sql: &str) -> bool {
    g.accepts(sql)
        .unwrap_or_else(|e| panic!("oracle errored on `{sql}`: {e}"))
}

#[test]
fn golden_accept_cases() {
    let g = Grammar::load(&grammar_path()).unwrap();
    let cases: &[&str] = &[
        "CREATE TABLE t (id INT)",
        "CREATE TABLE t (id INT PRIMARY KEY, name TEXT)",
        // CREATE INDEX requires ASYNC in DSQL, and the grammar models that.
        "CREATE INDEX ASYNC idx ON t (col)",
        "SELECT 1",
        "SELECT a, b FROM t WHERE id = 1",
        "INSERT INTO t (id, name) VALUES (1, 'x')",
        "UPDATE t SET name = 'y' WHERE id = 1",
        "DELETE FROM t WHERE id = 1",
        "CREATE VIEW v AS SELECT 1",
        "ALTER TABLE t ADD COLUMN x TEXT",
        "COMMIT",
        "START TRANSACTION",
    ];
    for sql in cases {
        assert!(case(&g, sql), "expected accept, grammar rejected: {sql}");
    }
}

#[test]
fn golden_reject_cases() {
    let g = Grammar::load(&grammar_path()).unwrap();
    let cases: &[&str] = &[
        "CRATE TABLE t (id INT)",
        "CREATE TABLE t (id INT",
        "INSERT INTO",
        "UPDATE",
        "CREATE TABLE",
    ];
    for sql in cases {
        assert!(!case(&g, sql), "expected reject, grammar accepted: {sql}");
    }
}

/// Empty / whitespace / comment-only input produces zero terminals; the
/// contract is that `accepts` returns `Ok(false)` (not `Err`, not panic).
#[test]
fn empty_input_returns_ok_false() {
    let g = Grammar::load(&grammar_path()).unwrap();
    for sql in ["", "   ", "\n\n", ";", "-- only a comment\n"] {
        let v = g
            .accepts(sql)
            .unwrap_or_else(|e| panic!("oracle errored on empty-ish input {sql:?}: {e}"));
        assert!(
            !v,
            "expected reject for empty-ish input {sql:?}, got accept"
        );
    }
}

/// A sqlparser tokenize failure (e.g. unterminated string literal) must
/// surface as `Err("tokenize: …")` rather than collapse to `Ok(false)`.
#[test]
fn accepts_propagates_tokenize_error() {
    let g = Grammar::load(&grammar_path()).unwrap();
    let err = g
        .accepts("SELECT 'unterminated")
        .expect_err("expected tokenize error");
    assert!(
        err.starts_with("tokenize:"),
        "expected tokenize: prefix, got {err:?}"
    );
}

fn expect_load_err(path: &std::path::Path) -> String {
    match Grammar::load(path) {
        Ok(_) => panic!("expected Grammar::load to fail for {}", path.display()),
        Err(e) => e,
    }
}

#[test]
fn load_fails_on_missing_file() {
    let path = PathBuf::from("/nonexistent/__definitely_not_a_grammar__.json");
    let err = expect_load_err(&path);
    assert!(
        err.starts_with("read grammar"),
        "expected read grammar prefix, got {err:?}"
    );
}

#[test]
fn load_fails_on_malformed_json() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("bad.json");
    std::fs::write(&path, "{ not json").expect("write tempfile");
    let err = expect_load_err(&path);
    assert!(
        err.starts_with("parse grammar"),
        "expected parse grammar prefix, got {err:?}"
    );
}

#[test]
fn load_fails_when_no_top_level_rules() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("empty.json");
    // A grammar with rules but none in TOP_LEVEL_RULES — Grammar::load must
    // refuse rather than build a Grammar with zero recognizers.
    std::fs::write(
        &path,
        r#"{"root":"Foo","rules":{"Foo":{"choices":[[{"text":"x","token_type":"Terminal"}]],"optional":false,"repetition":null}}}"#,
    )
    .expect("write tempfile");
    let err = expect_load_err(&path);
    assert!(
        err.starts_with("no top-level rules"),
        "expected no top-level rules prefix, got {err:?}"
    );
}
