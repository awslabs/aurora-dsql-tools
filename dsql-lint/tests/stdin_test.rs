use std::io::Write;
use std::process::{Command, Stdio};

fn dsql_lint_bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_dsql-lint"))
}

#[test]
fn lint_from_stdin() {
    let mut child = dsql_lint_bin()
        .arg("-")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();

    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"CREATE TABLE t (id SERIAL PRIMARY KEY);")
        .unwrap();

    let output = child.wait_with_output().unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("SERIAL"),
        "Should detect SERIAL from stdin: {stderr}"
    );
}

#[test]
fn lint_stdin_clean_sql() {
    let mut child = dsql_lint_bin()
        .arg("-")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();

    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"CREATE TABLE t (id UUID PRIMARY KEY);")
        .unwrap();

    let output = child.wait_with_output().unwrap();
    assert!(output.status.success());
}

#[test]
fn fix_from_stdin_to_stdout_text_mode() {
    let mut child = dsql_lint_bin()
        .arg("--fix")
        .arg("-")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();

    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"CREATE INDEX idx ON t(col);")
        .unwrap();

    let output = child.wait_with_output().unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("ASYNC"),
        "Fixed SQL should contain ASYNC on stdout in text mode: {stdout}"
    );
}

#[test]
fn fix_from_stdin_json_mode_embeds_fixed_sql() {
    let mut child = dsql_lint_bin()
        .arg("--fix")
        .arg("--format")
        .arg("json")
        .arg("-")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();

    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"CREATE INDEX idx ON t(col);")
        .unwrap();

    let output = child.wait_with_output().unwrap();
    assert!(output.status.success());
    assert!(output.stderr.is_empty(), "JSON mode must not write to stderr");

    let json: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("stdout should be valid JSON");

    let file_entry = &json["files"][0];
    assert_eq!(file_entry["file"], "<stdin>");

    // stdin fix mode: fixed_sql is populated, output_file is null
    let fixed_sql = file_entry["fixed_sql"]
        .as_str()
        .expect("stdin fix mode should include fixed_sql");
    assert!(
        fixed_sql.contains("ASYNC"),
        "fixed_sql should contain ASYNC: {fixed_sql}"
    );
    assert!(
        file_entry["output_file"].is_null(),
        "stdin fix mode should have output_file: null"
    );
}

#[test]
fn fix_from_stdin_to_output_file() {
    let dir = tempfile::tempdir().unwrap();
    let output_path = dir.path().join("out.sql");

    let mut child = dsql_lint_bin()
        .arg("--fix")
        .arg("-o")
        .arg(output_path.to_str().unwrap())
        .arg("-")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();

    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"CREATE INDEX idx ON t(col);")
        .unwrap();

    let output = child.wait_with_output().unwrap();
    assert!(output.status.success());
    let content = std::fs::read_to_string(&output_path).unwrap();
    assert!(content.contains("ASYNC"));
}

#[test]
fn stdin_json_lint_mode_shows_stdin_as_file() {
    let mut child = dsql_lint_bin()
        .arg("--format")
        .arg("json")
        .arg("-")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();

    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"CREATE TABLE t (id SERIAL PRIMARY KEY);")
        .unwrap();

    let output = child.wait_with_output().unwrap();
    let json: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("should be valid JSON");
    assert_eq!(json["files"][0]["file"], "<stdin>");
    assert!(!json["files"][0]["diagnostics"].as_array().unwrap().is_empty());
}

#[test]
fn no_args_shows_usage_and_exits() {
    let output = dsql_lint_bin()
        .stdin(Stdio::null())
        .output()
        .unwrap();

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Usage") || stderr.contains("usage") || stderr.contains("USAGE"),
        "Should show usage when no args: {stderr}"
    );
}
