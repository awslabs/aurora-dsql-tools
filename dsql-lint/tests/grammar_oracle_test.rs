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
    let produced: std::collections::BTreeSet<String> = PRODUCED_CHARCLASSES
        .iter()
        .map(|s| s.to_string())
        .collect();

    let missing: Vec<&String> = referenced.iter().filter(|c| !produced.contains(*c)).collect();
    assert!(
        missing.is_empty(),
        "Grammar references CharClass(es) the tokenizer doesn't produce: {missing:?}. \
         Teach `tokenize::map_token` to emit each and add the name to \
         `PRODUCED_CHARCLASSES`.",
    );
}

fn case(g: &Grammar, sql: &str) -> bool {
    g.accepts(sql).unwrap_or_else(|e| panic!("oracle errored on `{sql}`: {e}"))
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
