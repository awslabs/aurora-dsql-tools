//! Compares grammar acceptance to dsql-lint acceptance over the vendored
//! corpus and prints a per-statement diff.

use dsql_lint::grammar::{split_statements, Grammar};
use dsql_lint::{lint_sql, Diagnostic};
use std::path::{Path, PathBuf};

/// Each entry is a tax — every line in the corpus we've decided to ignore.
/// Past a small handful, the corpus or the tool needs fixing instead.
const SKIPPED_FILES: &[(&str, &str)] = &[];

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
    files_skipped: usize,
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
        let rel = match file.strip_prefix(&corpus_root) {
            Ok(p) => p.to_path_buf(),
            Err(_) => file.clone(),
        };
        let rel_str = rel.to_string_lossy();

        if let Some(reason) = SKIPPED_FILES
            .iter()
            .find_map(|(p, r)| (*p == rel_str).then_some(*r))
        {
            println!("== {} ==  SKIPPED ({})", rel_str, reason);
            totals.files_skipped += 1;
            continue;
        }

        totals.files_processed += 1;

        let raw = match std::fs::read_to_string(file) {
            Ok(s) => s,
            Err(e) => {
                println!("== {} ==  read error: {e}", rel_str);
                continue;
            }
        };

        let stmts = match split_statements(&raw) {
            Ok(v) => v,
            Err(e) => {
                println!("== {} ==\n  whole-file tokenize error: {e}\n", rel_str);
                totals.parse_error += 1;
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

            let grammar_verdict = grammar.accepts(&stmt.raw);
            match grammar_verdict {
                Err(_) => {
                    summary.parse_error += 1;
                    totals.parse_error += 1;
                    entries.push(Entry {
                        line: stmt.line,
                        category: Category::ParseError,
                        label,
                        diagnostics: lint_diags,
                        sql_preview: preview(&stmt.raw),
                    });
                }
                Ok(grammar_accepts) => {
                    let cat = match (lint_passes, grammar_accepts) {
                        (true, true) | (false, false) => Category::Agreement,
                        (true, false) => Category::LintTooLenient,
                        (false, true) => Category::LintTooStrict,
                    };
                    match cat {
                        Category::Agreement => {
                            summary.agreement += 1;
                            totals.agreement += 1;
                        }
                        Category::LintTooLenient => {
                            summary.lint_too_lenient += 1;
                            totals.lint_too_lenient += 1;
                            entries.push(Entry {
                                line: stmt.line,
                                category: cat,
                                label,
                                diagnostics: lint_diags,
                                sql_preview: preview(&stmt.raw),
                            });
                        }
                        Category::LintTooStrict => {
                            summary.lint_too_strict += 1;
                            totals.lint_too_strict += 1;
                            entries.push(Entry {
                                line: stmt.line,
                                category: cat,
                                label,
                                diagnostics: lint_diags,
                                sql_preview: preview(&stmt.raw),
                            });
                        }
                        Category::ParseError => unreachable!(),
                    }
                }
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
        "Summary: {} lint-too-strict, {} lint-too-lenient, {} parse-error  (files: {} processed, {} skipped, {} agreement statements)",
        totals.lint_too_strict,
        totals.lint_too_lenient,
        totals.parse_error,
        totals.files_processed,
        totals.files_skipped,
        totals.agreement,
    );
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
            println!("       grammar: parse error");
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
    let one_line: String = raw.lines().next().unwrap_or("").trim().chars().collect();
    if one_line.chars().count() > 120 {
        let truncated: String = one_line.chars().take(117).collect();
        format!("{truncated}...")
    } else {
        one_line
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
