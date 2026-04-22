use clap::Parser;
use std::path::{Path, PathBuf};
use std::process;

#[derive(Parser)]
#[command(name = "dsql-lint")]
#[command(about = "Lint SQL files for Aurora DSQL compatibility")]
struct Args {
    /// SQL files to lint
    files: Vec<String>,

    /// Fix mode: output DSQL-compatible SQL to a new file.
    /// Note: SQL comments in modified statements will not be preserved.
    #[arg(long)]
    fix: bool,

    /// Output file path (only with --fix and a single input file)
    #[arg(short, long)]
    output: Option<String>,
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
        eprintln!("Usage: dsql-lint <file.sql> [file2.sql ...]");
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
    let mut total_errors = 0;
    let mut had_read_error = false;

    for path in &args.files {
        let sql = match std::fs::read_to_string(path) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("Error reading '{path}': {e}");
                had_read_error = true;
                continue;
            }
        };

        let diagnostics = dsql_lint::lint::lint_sql(&sql);
        for d in &diagnostics {
            let preview: String = d.statement.chars().take(80).collect();
            eprintln!("{path}:{}: ERROR — {}", d.line, d.message);
            eprintln!("  → {}", d.suggestion);
            eprintln!("  | {preview}");
        }
        total_errors += diagnostics.len();
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
    use dsql_lint::lint::FixResult;

    let mut had_unfixable = false;
    let mut had_io_error = false;
    let mut unfixable_files: Vec<String> = Vec::new();
    let mut comment_note_printed = false;

    for path in &args.files {
        let sql = match std::fs::read_to_string(path) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("Error reading '{path}': {e}");
                had_io_error = true;
                continue;
            }
        };

        let result = dsql_lint::lint::fix_sql(&sql);

        let output_path = if let Some(ref o) = args.output {
            PathBuf::from(o)
        } else {
            default_output_path(path)
        };

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
            .zip(resolve(&output_path))
            .map(|(a, b)| a == b)
            .unwrap_or_else(|| input_path == output_path);
        if same_file {
            eprintln!(
                "Error: output path '{}' is the same as input '{}'. Use a different output path.",
                output_path.display(),
                path
            );
            process::exit(1);
        }

        let write_dir = output_path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        let write_result = tempfile::NamedTempFile::new_in(write_dir).and_then(|mut tmp| {
            use std::io::Write;
            tmp.write_all(result.sql.as_bytes())?;
            tmp.persist(&output_path)?;
            Ok(())
        });
        if let Err(e) = write_result {
            eprintln!("Error writing '{}': {e}", output_path.display());
            had_io_error = true;
            continue;
        }

        for d in &result.diagnostics {
            match &d.fix_result {
                FixResult::Fixed(msg) => {
                    eprintln!("{path}:{}: FIXED — {}", d.line, msg);
                }
                FixResult::FixedWithWarning(warning) => {
                    eprintln!("{path}:{}: WARNING — {}", d.line, warning);
                }
                FixResult::Unfixable => {
                    let preview: String = d.statement.chars().take(80).collect();
                    eprintln!("{path}:{}: ERROR (unfixable) — {}", d.line, d.message);
                    eprintln!("  → {}", d.suggestion);
                    eprintln!("  | {preview}");
                    if !had_unfixable || unfixable_files.last().map(|s| s.as_str()) != Some(path) {
                        unfixable_files.push(path.clone());
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

    if had_unfixable {
        eprintln!(
            "\nFix complete: {} file(s) had unfixable errors and require manual review.",
            unfixable_files.len()
        );
    }

    if had_unfixable || had_io_error {
        process::exit(1);
    }
}
