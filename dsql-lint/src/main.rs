use clap::Parser;
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

    for path in &args.files {
        let sql = std::fs::read_to_string(path).unwrap_or_else(|e| {
            eprintln!("Error reading '{path}': {e}");
            process::exit(1);
        });

        let diagnostics = dsql_lint::lint::lint_sql(&sql);
        for d in &diagnostics {
            let severity = if d.is_error { "ERROR" } else { "WARNING" };
            eprintln!("{path}:{}: {severity} — {}", d.line, d.message);
            eprintln!("  → {}", d.suggestion);
        }
        total_errors += diagnostics.iter().filter(|d| d.is_error).count();
    }

    if total_errors > 0 {
        eprintln!("\n{total_errors} error(s) found.");
        process::exit(1);
    } else if !args.files.is_empty() {
        eprintln!("All statements compatible with DSQL.");
    }
}
