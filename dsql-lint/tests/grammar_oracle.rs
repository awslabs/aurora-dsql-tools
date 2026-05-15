//! Grammar corpus oracle: walks `tests/grammar/{accept,reject,fixed}/` and
//! asserts dsql-lint behaviour matches each fixture's expectation. See
//! `tests/grammar/README.md` for fixture conventions.

mod grammar_corpus;

use dsql_lint::{fix_sql, lint_sql};

/// Walks the corpus and asserts dsql-lint matches each fixture's expectation.
/// Collects all failures into one report so a single grammar change that
/// invalidates many probes shows the full impact in one CI run.
#[test]
fn corpus_contract_test() {
    let fixtures = grammar_corpus::load_corpus();
    assert!(
        !fixtures.is_empty(),
        "corpus is empty — at least one fixture should exist"
    );

    let mut failures: Vec<String> = Vec::new();

    for fx in &fixtures {
        match fx.kind {
            grammar_corpus::FixtureKind::Accept | grammar_corpus::FixtureKind::Fixed => {
                let diags = lint_sql(&fx.body);
                if !diags.is_empty() {
                    failures.push(format!(
                        "{}: expected clean lint, got {} diagnostic(s):\n  {}",
                        fx.rel_path,
                        diags.len(),
                        diags
                            .iter()
                            .map(|d| format!("[{:?}] {}", d.rule, d.message))
                            .collect::<Vec<_>>()
                            .join("\n  ")
                    ));
                }
            }
            grammar_corpus::FixtureKind::Reject => {
                let diags = lint_sql(&fx.body);
                if diags.is_empty() {
                    failures.push(format!(
                        "{}: expected at least one diagnostic, got none. \
                         Either add a rule that catches this, or move the \
                         fixture to accept/ if it's now valid.",
                        fx.rel_path,
                    ));
                } else if let Some(expected_rule) = &fx.header.rule {
                    let observed: Vec<String> =
                        diags.iter().map(|d| format!("{:?}", d.rule)).collect();
                    let expected_pretty = grammar_corpus::snake_to_pascal(expected_rule);
                    if !observed.iter().any(|r| r == &expected_pretty) {
                        failures.push(format!(
                            "{}: header says rule '{expected_rule}' (LintRule::{expected_pretty}) \
                             should fire, but observed rules were [{}].",
                            fx.rel_path,
                            observed.join(", ")
                        ));
                    }
                }
            }
        }
    }

    let bless = std::env::var_os("BLESS").is_some();

    for fx in &fixtures {
        if fx.kind != grammar_corpus::FixtureKind::Reject {
            continue;
        }
        let Some(fix_target) = fx.header.fix.as_deref() else {
            continue;
        };
        let golden_path = grammar_corpus::corpus_root().join(fix_target);
        if !golden_path.exists() {
            failures.push(format!(
                "{}: fix: header points to '{fix_target}' but that file does not exist.",
                fx.rel_path,
            ));
            continue;
        }
        let actual = fix_sql(&fx.body).sql;

        if bless {
            let header = format!(
                "-- production: {}\n-- expectation: accept\n-- fixes: {}\n",
                fx.header.production, fx.rel_path,
            );
            let blessed = format!("{header}{actual}");
            std::fs::write(&golden_path, &blessed).expect("write golden");
            eprintln!("blessed {}", golden_path.display());
            continue;
        }

        // Compare against golden body (skip the header).
        let golden_text = std::fs::read_to_string(&golden_path).expect("read golden");
        let (_, golden_body_offset) =
            grammar_corpus::parse_header(&golden_text).expect("malformed golden header");
        let golden_body = &golden_text[golden_body_offset..];

        if golden_body != actual {
            failures.push(format!(
                "{}: fix output does not match golden {fix_target}.\n\
                 Expected:\n{golden_body}\n\
                 Actual:\n{actual}\n\
                 Run `BLESS=1 cargo test -p dsql-lint --test grammar_oracle` to update.",
                fx.rel_path
            ));
        }

        // Verify the back-reference points at this fixture. If the loader
        // did not pick up the golden (wrong directory, wrong extension,
        // or case mismatch), record a failure and continue so the rest of
        // the corpus still runs.
        let Some(golden_fixture) = fixtures.iter().find(|f| f.rel_path == *fix_target) else {
            failures.push(format!(
                "{}: fix: '{fix_target}' exists on disk but the loader did not pick it up \
                 (wrong directory, wrong extension, or case mismatch).",
                fx.rel_path,
            ));
            continue;
        };
        let expected_back_ref = &fx.rel_path;
        if golden_fixture.header.fixes.as_deref() != Some(expected_back_ref.as_str()) {
            failures.push(format!(
                "{}: fixes: back-reference is {:?}, expected {:?}",
                golden_fixture.rel_path, golden_fixture.header.fixes, expected_back_ref
            ));
        }
    }

    if !failures.is_empty() {
        panic!(
            "{} corpus fixture(s) failed:\n\n{}",
            failures.len(),
            failures.join("\n\n")
        );
    }
}

#[test]
fn corpus_coverage_test() {
    let ebnf_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("dsql-lint crate has a parent dir")
        .join("dsql_grammar.ebnf");
    let ebnf = std::fs::read_to_string(&ebnf_path)
        .unwrap_or_else(|e| panic!("read {}: {e}", ebnf_path.display()));
    let productions = grammar_corpus::extract_production_names(&ebnf);

    let fixtures = grammar_corpus::load_corpus();
    let covered: std::collections::BTreeSet<&str> = fixtures
        .iter()
        .map(|f| f.header.production.as_str())
        // Headers can list multiple comma-separated productions (e.g.
        // "CreateStmt, INHERITS clause"). Split on comma so each is counted.
        .flat_map(|p| p.split(',').map(|s| s.trim()))
        .collect();

    let mut uncovered: Vec<&String> = productions
        .iter()
        .filter(|p| !covered.contains(p.as_str()))
        .collect();
    uncovered.sort();

    eprintln!(
        "grammar coverage: {}/{} productions referenced by ≥1 fixture",
        productions.len() - uncovered.len(),
        productions.len()
    );
    if !uncovered.is_empty() {
        eprintln!("uncovered productions (informational):");
        for p in uncovered.iter().take(50) {
            eprintln!("  {p}");
        }
        if uncovered.len() > 50 {
            eprintln!("  ... and {} more", uncovered.len() - 50);
        }
    }

    // Also surface fixtures that reference productions no longer in the
    // EBNF (catches fixture drift after upstream renames).
    let valid: std::collections::BTreeSet<&str> = productions.iter().map(String::as_str).collect();
    let dangling: Vec<&str> = covered
        .iter()
        .copied()
        .filter(|p| !valid.contains(p))
        .collect();
    if !dangling.is_empty() {
        eprintln!("WARNING: fixtures reference unknown productions:");
        for p in &dangling {
            eprintln!("  {p}");
        }
    }
}
