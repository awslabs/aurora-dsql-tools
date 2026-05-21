//! Compares grammar acceptance to dsql-lint acceptance over the vendored
//! corpus and prints a per-statement diff.

use dsql_lint::grammar::{split_statements, Grammar};
use dsql_lint::{lint_sql, Diagnostic};
use std::path::{Path, PathBuf};

const GRAMMAR_REL: &str = "grammar/dsql_grammar.json";
const CORPUS_REL: &str = "tests/grammar_corpus";

#[derive(Debug, Clone, Copy)]
enum Category {
    Agreement,
    LintTooLenient,
    LintTooStrict,
    ParseError,
}

#[derive(Debug)]
struct Entry {
    line: usize,
    category: Category,
    label: Option<String>,
    diagnostics: Vec<Diagnostic>,
    /// Populated only for `ParseError` entries; carries the grammar
    /// tokenizer's error message so maintainers can distinguish a
    /// tokenizer crash from a recognizer rejection.
    grammar_error: Option<String>,
    sql_preview: String,
}

#[derive(Default, Debug)]
struct FileSummary {
    statements: usize,
    agreement: usize,
    lint_too_lenient: usize,
    lint_too_strict: usize,
    parse_error: usize,
}

#[derive(Default, Debug)]
struct Totals {
    files_processed: usize,
    file_read_errors: usize,
    file_tokenize_errors: usize,
    agreement: usize,
    lint_too_lenient: usize,
    lint_too_strict: usize,
    parse_error: usize,
}

fn main() {
    let manifest = env!("CARGO_MANIFEST_DIR");
    let grammar_path = PathBuf::from(manifest).join(GRAMMAR_REL);
    let corpus_root = PathBuf::from(manifest).join(CORPUS_REL);

    let grammar = match Grammar::load(&grammar_path) {
        Ok(g) => g,
        Err(e) => {
            eprintln!("error: load grammar from {}: {e}", grammar_path.display());
            std::process::exit(2);
        }
    };

    let corpus_files = match collect_corpus_files(&corpus_root) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("error: collect corpus from {}: {e}", corpus_root.display());
            std::process::exit(2);
        }
    };
    if corpus_files.is_empty() {
        eprintln!("error: no .sql files found under {}", corpus_root.display());
        std::process::exit(2);
    }

    let mut totals = Totals::default();
    for file in &corpus_files {
        let rel = file
            .strip_prefix(&corpus_root)
            .expect("walk only emits paths under corpus_root");
        let rel_str = rel.to_string_lossy();

        let raw = match std::fs::read_to_string(file) {
            Ok(s) => s,
            Err(e) => {
                println!("== {} ==  read error: {e}", rel_str);
                totals.file_read_errors += 1;
                continue;
            }
        };

        totals.files_processed += 1;

        let stmts = match split_statements(&raw) {
            Ok(v) => v,
            Err(e) => {
                println!("== {} ==\n  whole-file tokenize error: {e}\n", rel_str);
                totals.file_tokenize_errors += 1;
                continue;
            }
        };

        let labels = label_map(&raw);

        let mut summary = FileSummary::default();
        let mut entries = Vec::new();

        for stmt in &stmts {
            summary.statements += 1;

            let lint_diags: Vec<Diagnostic> = lint_sql(&stmt.raw);
            let lint_passes = lint_diags.is_empty();
            let label = labels
                .iter()
                .rev()
                .find_map(|(at_line, l)| (*at_line <= stmt.line).then(|| l.clone()));

            let (cat, grammar_error) = match grammar.accepts(&stmt.raw) {
                Err(e) => (Category::ParseError, Some(e)),
                Ok(true) if lint_passes => (Category::Agreement, None),
                Ok(false) if !lint_passes => (Category::Agreement, None),
                Ok(true) => (Category::LintTooStrict, None),
                Ok(false) => (Category::LintTooLenient, None),
            };
            bump(cat, &mut summary, &mut totals);
            if !matches!(cat, Category::Agreement) {
                entries.push(Entry {
                    line: stmt.line,
                    category: cat,
                    label,
                    diagnostics: lint_diags,
                    grammar_error,
                    sql_preview: preview(&stmt.raw),
                });
            }
        }

        println!("== {} ==", rel_str);
        println!(
            "  {} statements: {} lint-too-strict, {} lint-too-lenient, {} parse-error, {} agreement",
            summary.statements,
            summary.lint_too_strict,
            summary.lint_too_lenient,
            summary.parse_error,
            summary.agreement,
        );
        for e in &entries {
            print_entry(e);
        }
        println!();
    }

    println!(
        "Summary: {} lint-too-strict, {} lint-too-lenient, {} parse-error  \
         (files: {} processed, {} read-errors, {} tokenize-errors, {} agreement statements)",
        totals.lint_too_strict,
        totals.lint_too_lenient,
        totals.parse_error,
        totals.files_processed,
        totals.file_read_errors,
        totals.file_tokenize_errors,
        totals.agreement,
    );

    // Non-zero exit on file-level failures keeps CI honest if a future
    // pipeline runs grammar-diff against the corpus.
    if totals.file_read_errors > 0 || totals.file_tokenize_errors > 0 {
        std::process::exit(1);
    }
}

fn bump(cat: Category, summary: &mut FileSummary, totals: &mut Totals) {
    match cat {
        Category::Agreement => {
            summary.agreement += 1;
            totals.agreement += 1;
        }
        Category::LintTooLenient => {
            summary.lint_too_lenient += 1;
            totals.lint_too_lenient += 1;
        }
        Category::LintTooStrict => {
            summary.lint_too_strict += 1;
            totals.lint_too_strict += 1;
        }
        Category::ParseError => {
            summary.parse_error += 1;
            totals.parse_error += 1;
        }
    }
}

fn print_entry(e: &Entry) {
    let cat = match e.category {
        Category::LintTooStrict => "lint-too-strict",
        Category::LintTooLenient => "lint-too-lenient",
        Category::ParseError => "parse-error",
        Category::Agreement => unreachable!("agreement entries are not emitted"),
    };
    let label = e
        .label
        .as_deref()
        .map(|l| format!("  [label: {l}]"))
        .unwrap_or_default();
    println!();
    println!("  L{} {}{}", e.line, cat, label);
    match e.category {
        Category::LintTooStrict => {
            println!("       grammar: accepts");
            println!("       lint flagged:");
            for d in &e.diagnostics {
                println!("         - {:?}: {}", d.rule, d.message);
            }
        }
        Category::LintTooLenient => {
            println!("       lint: passes");
            println!("       grammar: rejects");
        }
        Category::ParseError => {
            match &e.grammar_error {
                Some(msg) => println!("       grammar: parse error: {msg}"),
                None => println!("       grammar: parse error"),
            }
            if !e.diagnostics.is_empty() {
                println!("       lint flagged:");
                for d in &e.diagnostics {
                    println!("         - {:?}: {}", d.rule, d.message);
                }
            }
        }
        Category::Agreement => {}
    }
    println!("       sql: {}", e.sql_preview);
}

fn preview(raw: &str) -> String {
    let one_line = raw.lines().next().unwrap_or("").trim();
    if one_line.chars().count() > 120 {
        let truncated: String = one_line.chars().take(117).collect();
        format!("{truncated}...")
    } else {
        one_line.to_string()
    }
}

fn collect_corpus_files(root: &Path) -> std::io::Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    walk(root, &mut out)?;
    out.sort();
    Ok(out)
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>) -> std::io::Result<()> {
    if !dir.exists() {
        return Ok(());
    }
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            walk(&path, out)?;
        } else if path.extension().is_some_and(|e| e == "sql") {
            out.push(path);
        }
    }
    Ok(())
}

/// Returns `(line, label)` pairs for every `-- label: …` comment in `raw`.
/// Used to attach the in-tree mirror's source-array label to each statement
/// in the diff output.
fn label_map(raw: &str) -> Vec<(usize, String)> {
    let mut out = Vec::new();
    for (idx, line) in raw.lines().enumerate() {
        let trimmed = line.trim_start();
        if let Some(rest) = trimmed.strip_prefix("--") {
            let body = rest.trim_start();
            if let Some(label_part) = body.strip_prefix("label:") {
                let label = label_part.trim().to_string();
                if !label.is_empty() {
                    out.push((idx + 1, label));
                }
            }
        }
    }
    out
}
