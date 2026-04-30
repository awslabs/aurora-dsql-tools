use clap::{Parser, ValueEnum};
use serde::Serialize;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process;

/// Version of the JSON output schema. Increment only on breaking changes
/// (renamed/removed fields, changed semantics). Additive changes keep the
/// same version.
const JSON_SCHEMA_VERSION: u32 = 1;

#[derive(Parser)]
#[command(name = "dsql-lint", version)]
#[command(about = "Lint SQL files for Aurora DSQL compatibility")]
#[command(after_help = "\
EXIT CODES:
  0  Clean (no issues) or all issues fixed without warnings
  1  Errors found (lint mode) or unfixable errors remain (fix mode)
  2  Usage error (invalid arguments)
  3  Fix mode only: all issues fixed, but some produced warnings

CI USAGE:
  Exit code 3 means the fix succeeded but produced warnings (e.g., removed
  foreign keys). In shell scripts with set -e or CI pipelines, handle it:

    dsql-lint --fix input.sql; rc=$?
    if [ $rc -eq 1 ]; then echo 'unfixable errors'; exit 1; fi
    # rc 0 or 3: fix succeeded (3 = review warnings)")]
struct Args {
    /// SQL files to lint (use '-' to read from stdin)
    files: Vec<String>,

    /// Fix mode: output DSQL-compatible SQL to a new file.
    /// Note: SQL comments in modified statements will not be preserved.
    #[arg(long)]
    fix: bool,

    /// Output file path (only with --fix and a single input file)
    #[arg(short, long)]
    output: Option<String>,

    /// Output format
    #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
    format: OutputFormat,
}

#[derive(Clone, Copy, Default, PartialEq, ValueEnum)]
enum OutputFormat {
    #[default]
    Text,
    Json,
}

#[derive(Serialize)]
struct JsonOutput {
    schema_version: u32,
    files: Vec<JsonFileOutput>,
    summary: JsonSummary,
}

#[derive(Serialize)]
struct JsonFileOutput {
    file: String,
    diagnostics: Vec<JsonDiagnostic>,
    error: Option<String>,
    output_file: Option<String>,
    fixed_sql: Option<String>,
}

#[derive(Serialize)]
struct JsonDiagnostic {
    #[serde(flatten)]
    diagnostic: dsql_lint::Diagnostic,
    statement_preview: String,
}

#[derive(Serialize)]
struct JsonSummary {
    errors: usize,
    warnings: usize,
    fixed: usize,
}

const STDIN_ARG: &str = "-";
const STDIN_DISPLAY: &str = "<stdin>";

fn is_stdin(path: &str) -> bool {
    path == STDIN_ARG
}

fn display_name(path: &str) -> &str {
    if is_stdin(path) {
        STDIN_DISPLAY
    } else {
        path
    }
}

fn read_source(path: &str) -> std::io::Result<String> {
    if is_stdin(path) {
        let mut buf = String::new();
        std::io::stdin().read_to_string(&mut buf).map_err(|e| {
            // read_to_string wraps non-UTF-8 bytes as InvalidData with an opaque
            // "stream did not contain valid UTF-8" message. Replace with something
            // more useful for CI logs.
            if e.kind() == std::io::ErrorKind::InvalidData {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "stdin contained non-UTF-8 bytes",
                )
            } else {
                e
            }
        })?;
        Ok(buf)
    } else {
        std::fs::read_to_string(path)
    }
}

fn make_preview(statement: &str) -> String {
    let collapsed: String = statement.split_whitespace().collect::<Vec<_>>().join(" ");
    collapsed.chars().take(80).collect()
}

fn default_output_path(input: &str) -> PathBuf {
    let p = Path::new(input);
    let stem = p.file_stem().unwrap_or_default().to_string_lossy();
    let ext = p
        .extension()
        .map(|e| e.to_string_lossy())
        .unwrap_or_default();
    let new_name = if ext.is_empty() {
        format!("{stem}-fixed")
    } else {
        format!("{stem}-fixed.{ext}")
    };
    p.with_file_name(new_name)
}

fn main() {
    let args = Args::parse();

    if args.files.is_empty() {
        eprintln!("Usage: dsql-lint <file.sql> [file2.sql ...] or dsql-lint - (read from stdin)");
        process::exit(1);
    }

    if args.output.is_some() && !args.fix {
        eprintln!("Error: -o/--output requires --fix");
        process::exit(1);
    }

    if args.output.is_some() && args.files.len() > 1 {
        eprintln!("Error: -o/--output can only be used with a single input file");
        process::exit(1);
    }

    if args.fix {
        run_fix(&args);
    } else {
        run_lint(&args);
    }
}

fn run_lint(args: &Args) {
    let json_mode = args.format == OutputFormat::Json;
    let mut total_errors = 0;
    let mut had_read_error = false;
    let mut json_files: Vec<JsonFileOutput> = Vec::new();

    for path in &args.files {
        let display = display_name(path);
        let sql = match read_source(path) {
            Ok(s) => s,
            Err(e) => {
                if json_mode {
                    json_files.push(JsonFileOutput {
                        file: path.clone(),
                        diagnostics: Vec::new(),
                        error: Some(format!("Error reading '{path}': {e}")),
                        output_file: None,
                        fixed_sql: None,
                    });
                } else {
                    eprintln!("Error reading '{path}': {e}");
                }
                had_read_error = true;
                continue;
            }
        };

        let diagnostics = dsql_lint::lint_sql(&sql);
        total_errors += diagnostics.len();

        if json_mode {
            let json_diags: Vec<JsonDiagnostic> = diagnostics
                .into_iter()
                .map(|d| JsonDiagnostic {
                    statement_preview: make_preview(&d.statement),
                    diagnostic: d,
                })
                .collect();
            json_files.push(JsonFileOutput {
                file: display.to_string(),
                diagnostics: json_diags,
                error: None,
                output_file: None,
                fixed_sql: None,
            });
        } else {
            for d in &diagnostics {
                eprintln!("{display}:{}: ERROR — {}", d.line, d.message);
                eprintln!("  → {}", d.suggestion);
                eprintln!("  | {}", make_preview(&d.statement));
            }
        }
    }

    if json_mode {
        let output = JsonOutput {
            schema_version: JSON_SCHEMA_VERSION,
            files: json_files,
            summary: JsonSummary {
                errors: total_errors,
                warnings: 0,
                fixed: 0,
            },
        };
        println!(
            "{}",
            serde_json::to_string_pretty(&output).expect("json serialization should not fail")
        );
        if total_errors > 0 || had_read_error {
            process::exit(1);
        }
        return;
    }

    if total_errors > 0 || had_read_error {
        if total_errors > 0 {
            eprintln!("\n{total_errors} error(s) found.");
        }
        process::exit(1);
    } else {
        eprintln!("All statements compatible with DSQL.");
    }
}

fn run_fix(args: &Args) {
    use dsql_lint::FixResult;

    let json_mode = args.format == OutputFormat::Json;
    let mut had_unfixable = false;
    let mut had_io_error = false;
    let mut unfixable_files: Vec<String> = Vec::new();
    let mut comment_note_printed = false;

    let mut json_files: Vec<JsonFileOutput> = Vec::new();
    let mut summary_errors: usize = 0;
    let mut summary_warnings: usize = 0;
    let mut summary_fixed: usize = 0;

    for path in &args.files {
        let display = display_name(path);
        let stdin_input = is_stdin(path);
        let sql = match read_source(path) {
            Ok(s) => s,
            Err(e) => {
                if json_mode {
                    json_files.push(JsonFileOutput {
                        file: path.clone(),
                        diagnostics: Vec::new(),
                        error: Some(format!("Error reading '{path}': {e}")),
                        output_file: None,
                        fixed_sql: None,
                    });
                } else {
                    eprintln!("Error reading '{path}': {e}");
                }
                had_io_error = true;
                continue;
            }
        };

        let result = dsql_lint::fix_sql(&sql);

        // Determine output destination.
        //   - Explicit --output: write to that path
        //   - stdin, no --output: don't write to disk (stream via stdout or JSON field)
        //   - file, no --output: write to default "<stem>-fixed.<ext>"
        let output_path: Option<PathBuf> = match (&args.output, stdin_input) {
            (Some(o), _) => Some(PathBuf::from(o)),
            (None, true) => None,
            (None, false) => Some(default_output_path(path)),
        };

        if let Some(ref output_path) = output_path {
            if !stdin_input {
                let resolve = |p: &Path| -> Option<PathBuf> {
                    let parent = p
                        .parent()
                        .filter(|p| !p.as_os_str().is_empty())
                        .unwrap_or(Path::new("."));
                    let file_name = p.file_name()?;
                    std::fs::canonicalize(parent)
                        .ok()
                        .map(|dir| dir.join(file_name))
                };
                let input_path = Path::new(path);
                let same_file = resolve(input_path)
                    .zip(resolve(output_path))
                    .map(|(a, b)| a == b)
                    .unwrap_or_else(|| input_path == output_path.as_path());
                if same_file {
                    let msg = format!(
                        "Error: output path '{}' is the same as input '{}'. Use a different output path.",
                        output_path.display(),
                        path
                    );
                    if json_mode {
                        json_files.push(JsonFileOutput {
                            file: display.to_string(),
                            diagnostics: Vec::new(),
                            error: Some(msg),
                            output_file: None,
                            fixed_sql: None,
                        });
                        emit_fix_json(json_files, summary_errors, summary_warnings, summary_fixed);
                        process::exit(1);
                    } else {
                        eprintln!("{msg}");
                        process::exit(1);
                    }
                }
            }

            let write_dir = output_path
                .parent()
                .filter(|p| !p.as_os_str().is_empty())
                .unwrap_or(Path::new("."));
            let write_result = tempfile::NamedTempFile::new_in(write_dir).and_then(|mut tmp| {
                use std::io::Write;
                tmp.write_all(result.sql.as_bytes())?;
                tmp.persist(output_path)?;
                Ok(())
            });
            if let Err(e) = write_result {
                if json_mode {
                    json_files.push(JsonFileOutput {
                        file: display.to_string(),
                        diagnostics: Vec::new(),
                        error: Some(format!("Error writing '{}': {e}", output_path.display())),
                        output_file: None,
                        fixed_sql: None,
                    });
                } else {
                    eprintln!("Error writing '{}': {e}", output_path.display());
                }
                had_io_error = true;
                continue;
            }
        }

        // Aggregate counts for JSON summary.
        for d in &result.diagnostics {
            match &d.fix_result {
                FixResult::Fixed(_) => summary_fixed += 1,
                FixResult::FixedWithWarning(_) => {
                    summary_fixed += 1;
                    summary_warnings += 1;
                }
                FixResult::Unfixable => summary_errors += 1,
            }
        }

        if json_mode {
            let json_diags: Vec<JsonDiagnostic> = result
                .diagnostics
                .iter()
                .map(|d| JsonDiagnostic {
                    statement_preview: make_preview(&d.statement),
                    diagnostic: d.clone(),
                })
                .collect();
            let has_unfixable = result
                .diagnostics
                .iter()
                .any(|d| matches!(d.fix_result, FixResult::Unfixable));
            if has_unfixable {
                had_unfixable = true;
                if unfixable_files.last().map(|s| s.as_str()) != Some(display) {
                    unfixable_files.push(display.to_string());
                }
            }
            // stdin + no --output: embed fixed SQL in JSON instead of a file path
            let fixed_sql = if stdin_input && output_path.is_none() {
                Some(result.sql.clone())
            } else {
                None
            };
            let output_file = output_path.as_ref().map(|p| p.display().to_string());
            json_files.push(JsonFileOutput {
                file: display.to_string(),
                diagnostics: json_diags,
                error: None,
                output_file,
                fixed_sql,
            });
            continue;
        }

        // --- Text-mode per-file reporting ---
        // stdin + no --output: stream fixed SQL to stdout
        if stdin_input && output_path.is_none() {
            print!("{}", result.sql);
        }

        for d in &result.diagnostics {
            match &d.fix_result {
                FixResult::Fixed(msg) => {
                    eprintln!("{display}:{}: FIXED — {}", d.line, msg);
                }
                FixResult::FixedWithWarning(warning) => {
                    eprintln!("{display}:{}: WARNING — {}", d.line, warning);
                }
                FixResult::Unfixable => {
                    eprintln!("{display}:{}: ERROR (unfixable) — {}", d.line, d.message);
                    eprintln!("  → {}", d.suggestion);
                    eprintln!("  | {}", make_preview(&d.statement));
                    if !had_unfixable || unfixable_files.last().map(|s| s.as_str()) != Some(display)
                    {
                        unfixable_files.push(display.to_string());
                    }
                    had_unfixable = true;
                }
            }
        }

        let unfixable_count = result
            .diagnostics
            .iter()
            .filter(|d| matches!(d.fix_result, FixResult::Unfixable))
            .count();
        let warning_count = result
            .diagnostics
            .iter()
            .filter(|d| matches!(d.fix_result, FixResult::FixedWithWarning(_)))
            .count();
        let had_any_fix = result.diagnostics.iter().any(|d| {
            matches!(
                d.fix_result,
                FixResult::Fixed(_) | FixResult::FixedWithWarning(_)
            )
        });

        if had_any_fix && !comment_note_printed && (sql.contains("--") || sql.contains("/*")) {
            eprintln!("Note: SQL comments in modified statements were not preserved.");
            comment_note_printed = true;
        }

        // Only emit file-location messages when we actually wrote a file.
        if let Some(ref output_path) = output_path {
            if result.sql.is_empty() {
                eprintln!(
                    "Fixed output is empty — all statements were removed: {}",
                    output_path.display()
                );
            } else if unfixable_count > 0 {
                eprintln!(
                    "Partial fix written to: {} ({unfixable_count} unfixable error(s) remain)",
                    output_path.display()
                );
            } else if warning_count > 0 {
                eprintln!(
                    "Fixed output written to: {} ({warning_count} warning(s) — review recommended)",
                    output_path.display()
                );
            } else {
                eprintln!("Fixed output written to: {}", output_path.display());
            }
        }
    }

    if json_mode {
        emit_fix_json(json_files, summary_errors, summary_warnings, summary_fixed);
        if had_unfixable || had_io_error {
            process::exit(1);
        }
        if summary_warnings > 0 {
            process::exit(3);
        }
        return;
    }

    if had_unfixable {
        eprintln!(
            "\nFix complete: {} file(s) had unfixable errors and require manual review.",
            unfixable_files.len()
        );
    }

    if had_unfixable || had_io_error {
        process::exit(1);
    }

    if summary_warnings > 0 {
        process::exit(3);
    }
}

fn emit_fix_json(files: Vec<JsonFileOutput>, errors: usize, warnings: usize, fixed: usize) {
    let output = JsonOutput {
        schema_version: JSON_SCHEMA_VERSION,
        files,
        summary: JsonSummary {
            errors,
            warnings,
            fixed,
        },
    };
    println!(
        "{}",
        serde_json::to_string_pretty(&output).expect("json serialization should not fail")
    );
}
