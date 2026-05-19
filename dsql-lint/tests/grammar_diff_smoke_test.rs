#![cfg(feature = "grammar-diff")]

use std::path::PathBuf;
use std::process::Command;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn binary_path() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_grammar-diff"))
}

#[test]
fn splitter_handles_simple_statements() {
    use dsql_lint::grammar::split_statements;

    let sql = "\
CREATE TABLE t (id INT);

ALTER TABLE t ADD COLUMN x TEXT;
SELECT * FROM t WHERE id = 1;
";
    let split = split_statements(sql).expect("split ok");
    assert_eq!(
        split.len(),
        3,
        "expected 3 statements, got {}: {split:?}",
        split.len()
    );
    assert!(split[0].raw.trim().starts_with("CREATE TABLE"));
    assert!(split[1].raw.trim().starts_with("ALTER TABLE"));
    assert!(split[2].raw.trim().starts_with("SELECT"));

    assert_eq!(split[0].line, 1);
    assert_eq!(split[1].line, 3);
    assert_eq!(split[2].line, 4);
}

/// Naive split-on-`;` would mis-split function bodies. The tokenizer puts
/// the body in a single `DollarQuotedString` token, hiding its semicolons.
#[test]
fn splitter_does_not_split_inside_dollar_quoted_string() {
    use dsql_lint::grammar::split_statements;

    let sql = "\
CREATE FUNCTION f() RETURNS INT AS $$
DECLARE x INT;
BEGIN
  x := 1;
  RETURN x;
END;
$$ LANGUAGE plpgsql;

SELECT 2;
";
    let split = split_statements(sql).expect("split ok");
    assert_eq!(
        split.len(),
        2,
        "dollar-quoted body must not split; got {} statements: {split:?}",
        split.len()
    );
    assert!(split[0].raw.contains("CREATE FUNCTION"));
    assert!(split[0].raw.contains("$$ LANGUAGE plpgsql"));
    assert!(split[1].raw.trim().starts_with("SELECT 2"));
}

#[test]
fn splitter_skips_empty_segments() {
    use dsql_lint::grammar::split_statements;

    let sql = ";; CREATE TABLE t (id INT); ;\n   ;\nSELECT 1;\n";
    let split = split_statements(sql).expect("split ok");
    let raws: Vec<String> = split.iter().map(|s| s.raw.trim().to_string()).collect();
    assert_eq!(
        raws,
        vec!["CREATE TABLE t (id INT);", "SELECT 1;"],
        "got: {raws:?}"
    );
}

/// If splitter `line`s and `lint_sql` diagnostic `line`s drift, the
/// per-statement diff is comparing different inputs. Whole-file diagnostics
/// must each map to exactly one split statement and reproduce in isolation.
#[test]
fn splitter_agrees_with_lint_sql_per_statement() {
    use dsql_lint::grammar::split_statements;
    use dsql_lint::lint_sql;

    let sql = "\
CREATE TABLE ok (id INT);
CREATE TABLE bad (id SERIAL);
ALTER TABLE ok ADD COLUMN x TEXT NOT NULL;
SELECT 1;
";
    let split = split_statements(sql).expect("split ok");
    let diags_full = lint_sql(sql);

    for d in &diags_full {
        let matching: Vec<_> = split.iter().filter(|s| s.line == d.line).collect();
        assert_eq!(
            matching.len(),
            1,
            "diagnostic at line {} did not match exactly one split statement (got {}): {d:?}",
            d.line,
            matching.len(),
        );
        // MultiDdlTransaction is intentionally cross-statement; everything
        // else must reproduce on the isolated statement.
        let single = lint_sql(&matching[0].raw);
        let cross_stmt_rule = matches!(d.rule, dsql_lint::LintRule::MultiDdlTransaction);
        if !cross_stmt_rule {
            assert!(
                single.iter().any(|s| s.rule == d.rule),
                "rule {:?} fired on whole file at line {} but not on the isolated statement\n  full diag: {:?}\n  isolated diags: {:?}\n  raw: {:?}",
                d.rule,
                d.line,
                d,
                single,
                matching[0].raw,
            );
        }
    }
}

#[test]
fn binary_runs_on_vendored_corpus() {
    let bin = binary_path();
    let out = Command::new(&bin)
        .current_dir(manifest_dir())
        .output()
        .expect("run grammar-diff");

    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);

    assert!(
        out.status.success(),
        "binary exited non-zero (status={:?})\nstdout:\n{stdout}\nstderr:\n{stderr}",
        out.status.code()
    );
    assert!(
        stdout.contains("Summary:"),
        "no Summary line in output:\n{stdout}"
    );
    assert!(
        stdout.contains("sqlfluff/") || stdout.contains("in_tree/"),
        "neither corpus subdir mentioned in output:\n{stdout}"
    );
}
