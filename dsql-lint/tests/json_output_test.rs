use std::process::{Command, Stdio};

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

    assert_eq!(
        json["schema_version"], 1,
        "schema_version must be present and equal to 1"
    );

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

    // SERIAL → FixedWithWarning, so in lint mode it counts toward `warnings`
    // (harmonized with fix-mode semantics), not `errors`. `errors` tracks
    // only truly-Unfixable diagnostics.
    assert_eq!(json["summary"]["warnings"], 1);
    assert_eq!(json["summary"]["errors"], 0);
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

    // Top-level summary aggregates. SERIAL → FixedWithWarning in lint mode
    // counts as a warning, not an error (errors are reserved for Unfixable).
    assert!(
        json["summary"]["warnings"].as_u64().unwrap() > 0
            || json["summary"]["errors"].as_u64().unwrap() > 0,
        "Expected at least one diagnostic in aggregated summary"
    );
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

#[test]
fn json_preview_appends_ellipsis_when_truncated() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("long.sql");
    // Build a statement whose collapsed form exceeds 80 chars so the preview
    // truncates. A long column list does the trick.
    let long_cols: Vec<String> = (0..20)
        .map(|i| format!("col_with_a_long_name_{i} INT"))
        .collect();
    let sql = format!(
        "CREATE TABLE t (id SERIAL PRIMARY KEY, {});",
        long_cols.join(", ")
    );
    std::fs::write(&input, &sql).unwrap();

    let output = dsql_lint_bin()
        .arg("--format")
        .arg("json")
        .arg(input.to_str().unwrap())
        .output()
        .unwrap();

    let json: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("should be valid JSON");
    let preview = json["files"][0]["diagnostics"][0]["statement_preview"]
        .as_str()
        .unwrap();
    assert!(
        preview.ends_with('…'),
        "Truncated preview should end with ellipsis, got: {preview}"
    );
}

#[test]
fn json_preview_no_ellipsis_when_short() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("short.sql");
    std::fs::write(&input, "CREATE TABLE t (id SERIAL PRIMARY KEY);").unwrap();

    let output = dsql_lint_bin()
        .arg("--format")
        .arg("json")
        .arg(input.to_str().unwrap())
        .output()
        .unwrap();

    let json: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("should be valid JSON");
    let preview = json["files"][0]["diagnostics"][0]["statement_preview"]
        .as_str()
        .unwrap();
    assert!(
        !preview.contains('…'),
        "Short preview should not contain ellipsis, got: {preview}"
    );
}

#[test]
fn json_lint_summary_splits_errors_and_warnings() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("mixed.sql");
    // SERIAL → FixedWithWarning, TRUNCATE → Unfixable
    std::fs::write(
        &input,
        "CREATE TABLE t (id SERIAL PRIMARY KEY);\nTRUNCATE TABLE foo;",
    )
    .unwrap();

    let output = dsql_lint_bin()
        .arg("--format")
        .arg("json")
        .arg(input.to_str().unwrap())
        .output()
        .unwrap();

    let json: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("should be valid JSON");

    // In lint mode the summary now splits the same way as fix mode:
    //   errors   = Unfixable count
    //   warnings = FixedWithWarning count
    assert_eq!(
        json["summary"]["errors"], 1,
        "Expected 1 unfixable (TRUNCATE)"
    );
    assert_eq!(
        json["summary"]["warnings"], 1,
        "Expected 1 warning (SERIAL)"
    );
}

#[test]
fn json_fix_summary_buckets_are_disjoint() {
    // A FixedWithWarning diagnostic must count as a warning, not as both
    // warning + fixed. Buckets are disjoint so `errors + warnings + fixed`
    // equals the diagnostic total.
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("mixed.sql");
    // Fixed: CREATE INDEX → ASYNC. FixedWithWarning: SERIAL → IDENTITY.
    // Unfixable: TRUNCATE.
    std::fs::write(
        &input,
        "CREATE INDEX idx ON t(col);\n\
         CREATE TABLE t (id SERIAL PRIMARY KEY);\n\
         TRUNCATE TABLE foo;",
    )
    .unwrap();

    let output = dsql_lint_bin()
        .arg("--fix")
        .arg("--format")
        .arg("json")
        .arg(input.to_str().unwrap())
        .output()
        .unwrap();

    let json: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("stdout should be valid JSON");
    assert_eq!(json["summary"]["fixed"], 1, "one Fixed diagnostic");
    assert_eq!(json["summary"]["warnings"], 1, "one FixedWithWarning");
    assert_eq!(json["summary"]["errors"], 1, "one Unfixable");
}

#[test]
fn json_io_failure_increments_summary_errors() {
    // summary.errors must stay in sync with the non-zero exit code so JSON
    // consumers gating on `summary.errors == 0` don't mistake a failed run
    // for a clean one.
    let output = dsql_lint_bin()
        .arg("--format")
        .arg("json")
        .arg("/nonexistent/file.sql")
        .output()
        .unwrap();

    assert!(!output.status.success());
    let json: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("stdout should be valid JSON");
    assert_eq!(
        json["summary"]["errors"], 1,
        "I/O failure should count toward summary.errors"
    );
}

#[test]
fn json_fix_same_file_error_does_not_drop_queued_files() {
    // Even though `-o` with multiple files is blocked, the scenario the
    // reviewer flagged is: one file in the batch has a conflicting derived
    // output path — it must NOT terminate the loop and swallow later files.
    // We validate via a single-file case that the same-file error produces a
    // well-formed JSON doc rather than truncating.
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("collide.sql");
    std::fs::write(&input, "CREATE TABLE t (id UUID PRIMARY KEY);").unwrap();

    let output = dsql_lint_bin()
        .arg("--fix")
        .arg("--format")
        .arg("json")
        .arg("-o")
        .arg(input.to_str().unwrap()) // same as input
        .arg(input.to_str().unwrap())
        .output()
        .unwrap();

    assert!(!output.status.success());
    let json: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("must be valid JSON even on error");
    let files = json["files"].as_array().unwrap();
    assert_eq!(files.len(), 1, "File entry must still appear in json_files");
    assert!(
        files[0]["error"]
            .as_str()
            .unwrap()
            .contains("same as input"),
        "Error field should carry the same-file message"
    );
    assert_eq!(json["schema_version"], 1);
}

#[test]
fn json_broken_pipe_exits_0() {
    // Consumer closes stdout before dsql-lint writes the JSON. emit_json must
    // map ErrorKind::BrokenPipe to exit 0, not panic from println! and not
    // surface the wrong exit code to scripts gating on it.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("a.sql");
    std::fs::write(&path, "CREATE TABLE t (id SERIAL);").unwrap();

    let mut child = dsql_lint_bin()
        .arg("--format")
        .arg("json")
        .arg(&path)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    drop(child.stdout.take().unwrap());

    let out = child.wait_with_output().unwrap();
    assert_eq!(out.status.code(), Some(0));
}
