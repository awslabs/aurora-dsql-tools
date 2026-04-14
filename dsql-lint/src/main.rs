use clap::Parser;
use dsql_lint::lint::Severity;
use std::process;

#[derive(Parser)]
#[command(name = "dsql-lint")]
#[command(about = "Lint SQL files for Aurora DSQL compatibility")]
struct Args {
    /// SQL files to lint
    files: Vec<String>,
}

fn main() {
    let args = Args::parse();

    if args.files.is_empty() {
        eprintln!("Usage: dsql-lint <file.sql> [file2.sql ...]");
        process::exit(1);
    }

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
            let severity = match d.severity {
                Severity::Error => "ERROR",
                Severity::Warning => "WARNING",
            };
            let preview: String = d.statement.chars().take(80).collect();
            eprintln!("{path}:{}: {severity} — {}", d.line, d.message);
            eprintln!("  → {}", d.suggestion);
            eprintln!("  | {preview}");
        }
        total_errors += diagnostics
            .iter()
            .filter(|d| d.severity == Severity::Error)
            .count();
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
