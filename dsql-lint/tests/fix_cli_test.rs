use std::process::Command;

fn dsql_lint_bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_dsql-lint"))
}

#[test]
fn fix_flag_creates_output_file() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("input.sql");
    std::fs::write(&input, "CREATE TABLE t (id UUID PRIMARY KEY);").unwrap();

    let status = dsql_lint_bin()
        .arg("--fix")
        .arg(input.to_str().unwrap())
        .status()
        .unwrap();

    assert!(status.success());
    let output = dir.path().join("input-fixed.sql");
    assert!(output.exists(), "Expected {output:?} to be created");
}

#[test]
fn fix_with_output_flag() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("input.sql");
    let output = dir.path().join("custom.sql");
    std::fs::write(&input, "CREATE TABLE t (id UUID PRIMARY KEY);").unwrap();

    let status = dsql_lint_bin()
        .arg("--fix")
        .arg("-o")
        .arg(output.to_str().unwrap())
        .arg(input.to_str().unwrap())
        .status()
        .unwrap();

    assert!(status.success());
    assert!(output.exists());
}

#[test]
fn output_without_fix_is_error() {
    let status = dsql_lint_bin()
        .arg("-o")
        .arg("out.sql")
        .arg("input.sql")
        .status()
        .unwrap();

    assert!(!status.success());
}

#[test]
fn output_with_multiple_files_is_error() {
    let dir = tempfile::tempdir().unwrap();
    let a = dir.path().join("a.sql");
    let b = dir.path().join("b.sql");
    std::fs::write(&a, "SELECT 1;").unwrap();
    std::fs::write(&b, "SELECT 2;").unwrap();

    let status = dsql_lint_bin()
        .arg("--fix")
        .arg("-o")
        .arg("out.sql")
        .arg(a.to_str().unwrap())
        .arg(b.to_str().unwrap())
        .status()
        .unwrap();

    assert!(!status.success());
}

#[test]
fn fix_serial_produces_identity_in_output() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("input.sql");
    std::fs::write(&input, "CREATE TABLE t (id SERIAL PRIMARY KEY);").unwrap();

    let status = dsql_lint_bin()
        .arg("--fix")
        .arg(input.to_str().unwrap())
        .status()
        .unwrap();

    assert!(status.success());
    let output = dir.path().join("input-fixed.sql");
    let content = std::fs::read_to_string(&output).unwrap();
    assert!(
        content.contains("BIGINT"),
        "Expected BIGINT in output: {content}"
    );
    assert!(
        content.contains("IDENTITY"),
        "Expected IDENTITY in output: {content}"
    );
    assert!(
        !content.to_uppercase().contains("SERIAL"),
        "Should not contain SERIAL: {content}"
    );
}

#[test]
fn fix_unfixable_returns_exit_code_1() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("input.sql");
    std::fs::write(&input, "TRUNCATE TABLE foo;").unwrap();

    let status = dsql_lint_bin()
        .arg("--fix")
        .arg(input.to_str().unwrap())
        .status()
        .unwrap();

    assert!(
        !status.success(),
        "Should exit 1 when unfixable errors remain"
    );
}

#[test]
fn fix_output_same_as_input_is_error() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("input.sql");
    std::fs::write(&input, "CREATE TABLE t (id SERIAL PRIMARY KEY);").unwrap();

    let output = dsql_lint_bin()
        .arg("--fix")
        .arg("-o")
        .arg(input.to_str().unwrap())
        .arg(input.to_str().unwrap())
        .output()
        .unwrap();

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("same as input"),
        "Expected 'same as input' error, got: {stderr}"
    );
}

#[test]
fn fix_unfixable_shows_partial_message() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("input.sql");
    std::fs::write(&input, "TRUNCATE TABLE foo;").unwrap();

    let output = dsql_lint_bin()
        .arg("--fix")
        .arg(input.to_str().unwrap())
        .output()
        .unwrap();

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Partial fix written to"),
        "Expected 'Partial fix' message, got: {stderr}"
    );
    assert!(
        stderr.contains("unfixable error(s) remain"),
        "Expected unfixable count in message, got: {stderr}"
    );
}

#[test]
fn fix_fk_removal_shows_warning_count() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("input.sql");
    std::fs::write(&input, "CREATE TABLE t (id INT, cid INT REFERENCES c(id));").unwrap();

    let output = dsql_lint_bin()
        .arg("--fix")
        .arg(input.to_str().unwrap())
        .output()
        .unwrap();

    assert!(output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("warning(s)"),
        "Expected warning count in message, got: {stderr}"
    );
    assert!(
        stderr.contains("review recommended"),
        "Expected 'review recommended' in message, got: {stderr}"
    );
    assert!(
        !stderr.contains("SQL comments"),
        "Comment note should not appear when input has no comments, got: {stderr}"
    );
}

#[test]
fn fix_clean_file_shows_plain_message() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("input.sql");
    std::fs::write(&input, "CREATE TABLE t (id UUID PRIMARY KEY);").unwrap();

    let output = dsql_lint_bin()
        .arg("--fix")
        .arg(input.to_str().unwrap())
        .output()
        .unwrap();

    assert!(output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Fixed output written to:"),
        "Expected success message, got: {stderr}"
    );
    assert!(
        !stderr.contains("Partial") && !stderr.contains("warning") && !stderr.contains("comments"),
        "Clean file should not show warnings, partial, or comment note, got: {stderr}"
    );
}

#[test]
fn fix_empty_output_shows_distinct_message() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("input.sql");
    std::fs::write(
        &input,
        "ALTER TABLE t ADD CONSTRAINT fk_c FOREIGN KEY (cid) REFERENCES c(id);",
    )
    .unwrap();

    let output = dsql_lint_bin()
        .arg("--fix")
        .arg(input.to_str().unwrap())
        .output()
        .unwrap();

    assert!(output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Fixed output is empty"),
        "Expected empty-output message, got: {stderr}"
    );
}

#[test]
fn fix_comment_note_only_when_comments_present() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("input.sql");
    std::fs::write(
        &input,
        "-- migration\nCREATE TABLE t (id SERIAL PRIMARY KEY);",
    )
    .unwrap();

    let output = dsql_lint_bin()
        .arg("--fix")
        .arg(input.to_str().unwrap())
        .output()
        .unwrap();

    assert!(output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("SQL comments in modified statements were not preserved"),
        "Expected comment note when input has comments, got: {stderr}"
    );
}

#[test]
fn fix_unfixable_shows_summary() {
    let dir = tempfile::tempdir().unwrap();
    let a = dir.path().join("a.sql");
    let b = dir.path().join("b.sql");
    std::fs::write(&a, "TRUNCATE TABLE foo;").unwrap();
    std::fs::write(&b, "TRUNCATE TABLE bar;").unwrap();

    let output = dsql_lint_bin()
        .arg("--fix")
        .arg(a.to_str().unwrap())
        .arg(b.to_str().unwrap())
        .output()
        .unwrap();

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("2 file(s) had unfixable errors"),
        "Expected multi-file summary, got: {stderr}"
    );
}

#[test]
fn fix_multiple_files_creates_multiple_outputs() {
    let dir = tempfile::tempdir().unwrap();
    let a = dir.path().join("a.sql");
    let b = dir.path().join("b.sql");
    std::fs::write(&a, "CREATE INDEX idx ON t(col);").unwrap();
    std::fs::write(&b, "CREATE TABLE t (id UUID PRIMARY KEY);").unwrap();

    let status = dsql_lint_bin()
        .arg("--fix")
        .arg(a.to_str().unwrap())
        .arg(b.to_str().unwrap())
        .status()
        .unwrap();

    assert!(status.success());
    assert!(dir.path().join("a-fixed.sql").exists());
    assert!(dir.path().join("b-fixed.sql").exists());
}
