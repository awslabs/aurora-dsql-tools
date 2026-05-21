#![cfg(feature = "grammar-diff")]

use dsql_lint::grammar::tokenize::PRODUCED_CHARCLASSES;
use dsql_lint::grammar::Grammar;
use std::path::PathBuf;

fn grammar_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("grammar/dsql_grammar.json")
}

/// `Grammar::load` warns once per asymmetry between `TOP_LEVEL_RULES` and
/// the grammar JSON, plus once for non-terminals referenced but not
/// defined. Catches silent drift on grammar refresh.
#[test]
fn load_warns_on_real_grammar_asymmetries() {
    let mut warnings: Vec<String> = Vec::new();
    let _ = dsql_lint::grammar::Grammar::load_with_warnings(&grammar_path(), |w| {
        warnings.push(w.to_string())
    })
    .unwrap();
    // Today's grammar has 6 *Stmt rules outside TOP_LEVEL_RULES (e.g.
    // PreparableStmt) plus 3 referenced-but-undefined non-terminals. If a
    // future refresh resolves these, the test should be updated to assert
    // *no* warnings — that would be the cleaner state.
    assert!(
        warnings
            .iter()
            .any(|w| w.contains("not in TOP_LEVEL_RULES")),
        "expected `*Stmt` asymmetry warning, got: {warnings:?}"
    );
    assert!(
        warnings
            .iter()
            .any(|w| w.contains("referenced but not defined")),
        "expected undefined-nonterm warning, got: {warnings:?}"
    );
}

/// Targeted regression for the keyword-demotion path. A breakage that
/// disabled demotion entirely would surface here as a missing demotion for
/// `id`, before any golden case starts failing in confusing ways.
///
/// Why `id`: sqlparser-dsql classifies `id` as `Keyword::ID`, but DSQL's
/// grammar doesn't list `ID` as a Terminal anywhere, so it must be demoted
/// to `IDENT` for the recognizer to accept normal identifier positions.
#[test]
fn keyword_demotion_path_demotes_id() {
    let g = Grammar::load(&grammar_path()).unwrap();
    let (accepts, demotions) = g
        .accepts_with_demotions("CREATE TABLE t (id INT)")
        .expect("accept ok");
    assert!(accepts, "demotion broken: CREATE TABLE t (id INT) rejected");
    assert!(
        demotions.iter().any(|k| k == "ID"),
        "expected `ID` in demotions, got {demotions:?}"
    );
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

/// Empty / whitespace / comment-only input has zero terminals after the
/// Skip filter; the contract is that `accepts` returns `Err`, not `Ok(false)`.
/// Routing to `parse-error` instead of `lint-too-lenient` makes a future
/// tokenizer regression that misclassifies everything as `Skip` visible.
#[test]
fn empty_input_returns_err() {
    let g = Grammar::load(&grammar_path()).unwrap();
    for sql in ["", "   ", "\n\n", ";", "-- only a comment\n"] {
        let err = g
            .accepts(sql)
            .err()
            .unwrap_or_else(|| panic!("expected Err on empty-ish input {sql:?}"));
        assert!(
            err.contains("no terminals"),
            "unexpected error on {sql:?}: {err}"
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
