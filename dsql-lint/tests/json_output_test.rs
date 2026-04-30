use std::process::Command;

fn dsql_lint_bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_dsql-lint"))
}

#[test]
fn json_lint_clean_file() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("clean.sql");
    std::fs::write(&input, "CREATE TABLE t (id UUID PRIMARY KEY);").unwrap();

    let output = dsql_lint_bin()
        .arg("--format")
        .arg("json")
        .arg(input.to_str().unwrap())
        .output()
        .unwrap();

    assert!(output.status.success());
    assert!(
        output.stderr.is_empty(),
        "JSON mode must not write to stderr: {:?}",
        String::from_utf8_lossy(&output.stderr)
    );

    let json: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("stdout should be valid JSON");

    let files = json["files"].as_array().expect("should have 'files' array");
    assert_eq!(files.len(), 1);

    let diags = files[0]["diagnostics"].as_array().unwrap();
    assert!(diags.is_empty());
    assert_eq!(json["summary"]["errors"], 0);
    assert_eq!(json["summary"]["warnings"], 0);
}

#[test]
fn json_lint_with_errors() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("bad.sql");
    std::fs::write(&input, "CREATE TABLE t (id SERIAL PRIMARY KEY);").unwrap();

    let output = dsql_lint_bin()
        .arg("--format")
        .arg("json")
        .arg(input.to_str().unwrap())
        .output()
        .unwrap();

    assert!(!output.status.success());
    assert!(
        output.stderr.is_empty(),
        "JSON mode must not write to stderr"
    );

    let json: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("stdout should be valid JSON");

    let files = json["files"].as_array().unwrap();
    assert_eq!(files.len(), 1);
    assert!(
        files[0]["file"].as_str().unwrap().contains("bad.sql"),
        "Expected file field to contain filename"
    );

    let diags = files[0]["diagnostics"].as_array().unwrap();
    assert!(!diags.is_empty());

    let d = &diags[0];
    assert_eq!(d["rule"], "serial_type");
    assert!(d["line"].is_number());
    assert!(d["message"].as_str().unwrap().contains("SERIAL"));
    assert!(d["suggestion"].as_str().is_some());
    assert!(d["fix_result"]["status"].as_str().is_some());

    // statement_preview should be whitespace-collapsed
    let preview = d["statement_preview"].as_str().unwrap();
    assert!(
        !preview.contains('\n'),
        "Preview should not contain newlines"
    );

    assert_eq!(json["summary"]["errors"], 1);
}

#[test]
fn json_lint_multiline_preview_collapsed() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("multiline.sql");
    std::fs::write(
        &input,
        "CREATE TABLE t (\n    id SERIAL\n    PRIMARY KEY\n);",
    )
    .unwrap();

    let output = dsql_lint_bin()
        .arg("--format")
        .arg("json")
        .arg(input.to_str().unwrap())
        .output()
        .unwrap();

    let json: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("stdout should be valid JSON");
    let preview = json["files"][0]["diagnostics"][0]["statement_preview"]
        .as_str()
        .unwrap();
    assert!(
        !preview.contains('\n'),
        "Multi-line SQL preview should be collapsed to single line: {preview}"
    );
}

#[test]
fn json_fix_mode_file_has_output_file() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("fix.sql");
    std::fs::write(&input, "CREATE INDEX idx ON t(col);").unwrap();

    let output = dsql_lint_bin()
        .arg("--fix")
        .arg("--format")
        .arg("json")
        .arg(input.to_str().unwrap())
        .output()
        .unwrap();

    assert!(output.status.success());
    assert!(
        output.stderr.is_empty(),
        "JSON mode must not write to stderr"
    );

    let json: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("stdout should be valid JSON");

    let file_entry = &json["files"][0];
    let diags = file_entry["diagnostics"].as_array().unwrap();
    assert!(!diags.is_empty());
    assert_eq!(diags[0]["fix_result"]["status"], "fixed");

    // File mode: output_file is populated, fixed_sql is null
    assert!(
        file_entry["output_file"].as_str().is_some(),
        "File fix mode should include output_file path"
    );
    assert!(
        file_entry["fixed_sql"].is_null(),
        "File fix mode should have fixed_sql: null"
    );
    assert!(
        file_entry["error"].is_null(),
        "Successful file should have error: null"
    );

    assert_eq!(json["summary"]["fixed"], 1);
}

#[test]
fn json_file_read_error_appears_in_files_array() {
    let output = dsql_lint_bin()
        .arg("--format")
        .arg("json")
        .arg("/nonexistent/file.sql")
        .output()
        .unwrap();

    assert!(!output.status.success());
    assert!(
        output.stderr.is_empty(),
        "JSON mode must not write to stderr"
    );

    let json: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("stdout should be valid JSON");

    let files = json["files"].as_array().unwrap();
    assert_eq!(files.len(), 1);
    assert_eq!(files[0]["file"], "/nonexistent/file.sql");
    assert!(
        files[0]["error"].as_str().is_some(),
        "Should have error message for unreadable file"
    );
    assert!(
        files[0]["diagnostics"].as_array().unwrap().is_empty(),
        "Unreadable file should have empty diagnostics"
    );
}

#[test]
fn json_lint_nullable_fields_always_present() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("clean.sql");
    std::fs::write(&input, "CREATE TABLE t (id UUID PRIMARY KEY);").unwrap();

    let output = dsql_lint_bin()
        .arg("--format")
        .arg("json")
        .arg(input.to_str().unwrap())
        .output()
        .unwrap();

    let json: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("stdout should be valid JSON");
    let file_entry = &json["files"][0];

    // All nullable fields should be present as null (not absent)
    assert!(
        file_entry.get("error").is_some(),
        "error field must be present"
    );
    assert!(
        file_entry["error"].is_null(),
        "error should be null for clean file"
    );
    assert!(
        file_entry.get("output_file").is_some(),
        "output_file field must be present"
    );
    assert!(
        file_entry["output_file"].is_null(),
        "output_file should be null in lint mode"
    );
    assert!(
        file_entry.get("fixed_sql").is_some(),
        "fixed_sql field must be present"
    );
    assert!(
        file_entry["fixed_sql"].is_null(),
        "fixed_sql should be null in lint mode"
    );
}

#[test]
fn json_format_invalid_value_is_error() {
    let output = dsql_lint_bin()
        .arg("--format")
        .arg("xml")
        .arg("dummy.sql")
        .output()
        .unwrap();

    assert!(!output.status.success());
}

#[test]
fn json_multiple_files_grouped() {
    let dir = tempfile::tempdir().unwrap();
    let a = dir.path().join("a.sql");
    let b = dir.path().join("b.sql");
    std::fs::write(&a, "CREATE TABLE t (id SERIAL PRIMARY KEY);").unwrap();
    std::fs::write(&b, "CREATE TABLE u (id UUID PRIMARY KEY);").unwrap();

    let output = dsql_lint_bin()
        .arg("--format")
        .arg("json")
        .arg(a.to_str().unwrap())
        .arg(b.to_str().unwrap())
        .output()
        .unwrap();

    assert!(
        output.stderr.is_empty(),
        "JSON mode must not write to stderr"
    );

    let json: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("should be valid JSON");

    let files = json["files"].as_array().expect("should have 'files' array");
    assert_eq!(files.len(), 2);

    // First file has errors
    assert!(!files[0]["diagnostics"].as_array().unwrap().is_empty());
    assert_eq!(files[0]["file"].as_str().unwrap(), a.to_str().unwrap());

    // Second file is clean
    assert!(files[1]["diagnostics"].as_array().unwrap().is_empty());
    assert_eq!(files[1]["file"].as_str().unwrap(), b.to_str().unwrap());

    // Top-level summary aggregates
    assert!(json["summary"]["errors"].as_u64().unwrap() > 0);
}

#[test]
fn json_single_file_still_has_files_array() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("single.sql");
    std::fs::write(&input, "CREATE TABLE t (id UUID PRIMARY KEY);").unwrap();

    let output = dsql_lint_bin()
        .arg("--format")
        .arg("json")
        .arg(input.to_str().unwrap())
        .output()
        .unwrap();

    let json: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("should be valid JSON");
    assert_eq!(json["files"].as_array().unwrap().len(), 1);
}
