use std::process::Command;

fn dsql_lint_bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_dsql-lint"))
}

#[test]
fn fix_clean_exits_0() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("clean.sql");
    std::fs::write(&input, "CREATE TABLE t (id UUID PRIMARY KEY);").unwrap();

    let status = dsql_lint_bin()
        .arg("--fix")
        .arg(input.to_str().unwrap())
        .status()
        .unwrap();

    assert_eq!(status.code(), Some(0));
}

#[test]
fn fix_all_fixed_no_warnings_exits_0() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("fixable.sql");
    std::fs::write(&input, "CREATE INDEX idx ON t(col);").unwrap();

    let status = dsql_lint_bin()
        .arg("--fix")
        .arg(input.to_str().unwrap())
        .status()
        .unwrap();

    assert_eq!(status.code(), Some(0));
}

#[test]
fn fix_with_warnings_exits_3() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("warn.sql");
    // FK removal produces FixedWithWarning
    std::fs::write(&input, "CREATE TABLE t (id INT, cid INT REFERENCES c(id));").unwrap();

    let status = dsql_lint_bin()
        .arg("--fix")
        .arg(input.to_str().unwrap())
        .status()
        .unwrap();

    assert_eq!(status.code(), Some(3));
}

#[test]
fn fix_serial_exits_3() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("serial.sql");
    // SERIAL → FixedWithWarning
    std::fs::write(&input, "CREATE TABLE t (id SERIAL PRIMARY KEY);").unwrap();

    let status = dsql_lint_bin()
        .arg("--fix")
        .arg(input.to_str().unwrap())
        .status()
        .unwrap();

    assert_eq!(status.code(), Some(3));
}

#[test]
fn fix_unfixable_exits_1() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("unfixable.sql");
    std::fs::write(&input, "TRUNCATE TABLE foo;").unwrap();

    let status = dsql_lint_bin()
        .arg("--fix")
        .arg(input.to_str().unwrap())
        .status()
        .unwrap();

    assert_eq!(status.code(), Some(1));
}

#[test]
fn fix_mixed_unfixable_and_warning_exits_1() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("mixed.sql");
    // FK removal (warning) + TRUNCATE (unfixable) — unfixable takes precedence
    std::fs::write(
        &input,
        "CREATE TABLE t (id INT, cid INT REFERENCES c(id));\nTRUNCATE TABLE foo;",
    )
    .unwrap();

    let status = dsql_lint_bin()
        .arg("--fix")
        .arg(input.to_str().unwrap())
        .status()
        .unwrap();

    assert_eq!(status.code(), Some(1));
}

#[test]
fn lint_mode_still_uses_0_and_1() {
    let dir = tempfile::tempdir().unwrap();

    let clean = dir.path().join("clean.sql");
    std::fs::write(&clean, "CREATE TABLE t (id UUID PRIMARY KEY);").unwrap();
    let status = dsql_lint_bin()
        .arg(clean.to_str().unwrap())
        .status()
        .unwrap();
    assert_eq!(status.code(), Some(0));

    let bad = dir.path().join("bad.sql");
    std::fs::write(&bad, "CREATE TABLE t (id SERIAL PRIMARY KEY);").unwrap();
    let status = dsql_lint_bin().arg(bad.to_str().unwrap()).status().unwrap();
    assert_eq!(status.code(), Some(1));
}

#[test]
fn clap_usage_error_exits_2() {
    let output = dsql_lint_bin().arg("--invalid-flag").output().unwrap();

    assert_eq!(
        output.status.code(),
        Some(2),
        "clap usage errors should exit 2 (distinct from our exit 3)"
    );
}

#[test]
fn output_without_fix_exits_2() {
    // Our own argv validation (-o requires --fix) must exit 2, matching clap's
    // convention and the EXIT CODES table in --help. Previously returned 1,
    // which misclassifies user misuse as a lint failure in CI scripts that
    // check `if [ $rc -eq 1 ]`.
    let output = dsql_lint_bin()
        .arg("-o")
        .arg("out.sql")
        .arg("input.sql")
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(2));
}

#[test]
fn no_files_exits_2() {
    let output = dsql_lint_bin().output().unwrap();
    assert_eq!(output.status.code(), Some(2));
}

#[test]
fn lint_text_labels_match_fix_result_variant() {
    // Text-mode severity labels must match the JSON summary split:
    //   Unfixable        → ERROR
    //   FixedWithWarning → WARNING
    //   Fixed            → INFO
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("mixed.sql");
    // SERIAL → FixedWithWarning (WARNING)
    // CREATE INDEX without ASYNC → Fixed (INFO)
    // TRUNCATE → Unfixable (ERROR)
    std::fs::write(
        &input,
        "CREATE TABLE t (id SERIAL PRIMARY KEY);\n\
         CREATE INDEX idx ON t(id);\n\
         TRUNCATE TABLE t;",
    )
    .unwrap();

    let output = dsql_lint_bin()
        .arg(input.to_str().unwrap())
        .output()
        .unwrap();

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains(": ERROR —"),
        "expected ERROR label: {stderr}"
    );
    assert!(
        stderr.contains(": WARNING —"),
        "expected WARNING label: {stderr}"
    );
    assert!(stderr.contains(": INFO —"), "expected INFO label: {stderr}");
}

#[test]
fn lint_mode_exits_1_on_fixed_only_diagnostics() {
    // Regression: CREATE INDEX without ASYNC produces a diagnostic with
    // fix_result = Fixed. The harmonized summary split counts it under
    // neither errors nor warnings, but lint mode must still exit 1 — the
    // input is not DSQL-compatible as written.
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("fixable.sql");
    std::fs::write(&input, "CREATE INDEX idx ON t(col);").unwrap();

    let status = dsql_lint_bin()
        .arg(input.to_str().unwrap())
        .status()
        .unwrap();

    assert_eq!(
        status.code(),
        Some(1),
        "lint mode must exit 1 on any DSQL incompatibility, even when all diagnostics are Fixed-rated"
    );
}
