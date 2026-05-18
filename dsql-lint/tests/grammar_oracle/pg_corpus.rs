//! Loader for the vendored Postgres regression-test corpus under
//! `tests/grammar_oracle/pg_corpus/`.
//!
//! Files are upstream Postgres regression `.sql` scripts. They contain
//! psql metacommands, intentionally-invalid SQL marked `-- fail`, and
//! multi-statement scripts. The loader:
//!
//! - splits each file into top-level statements using `sqlparser`'s
//!   tokenizer (same shape as the production `split_statements` in
//!   `src/lint.rs`)
//! - drops psql `\…` lines (the tokenizer chokes on them and they
//!   aren't SQL anyway)
//! - returns a flat `Vec<&'static str>` of statements ready for the
//!   drift oracle. Each statement keeps its trailing semicolon for
//!   parity with our other corpus arrays.
//!
//! Refresh via `dsql-lint/scripts/refresh_pg_corpus.sh`.

use sqlparser::dialect::PostgreSqlDialect;
use sqlparser::tokenizer::{Token, Tokenizer};
use std::path::Path;
use std::sync::OnceLock;

/// All vendored statements, lazily loaded once.
pub fn statements() -> &'static [String] {
    static CACHED: OnceLock<Vec<String>> = OnceLock::new();
    CACHED.get_or_init(load_all)
}

fn load_all() -> Vec<String> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("grammar_oracle")
        .join("pg_corpus");
    let mut entries: Vec<_> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("read_dir {}: {e}", dir.display()))
        .filter_map(|r| r.ok())
        .filter(|e| e.path().extension().map(|x| x == "sql").unwrap_or(false))
        .collect();
    entries.sort_by_key(|e| e.path()); // deterministic order

    let mut out = Vec::new();
    for entry in entries {
        let path = entry.path();
        let raw = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
        let cleaned = strip_psql_metacommands(&raw);
        match split_statements(&cleaned) {
            Ok(stmts) => out.extend(stmts),
            Err(e) => panic!("tokenize {}: {e}", path.display()),
        }
    }
    out
}

/// Drop lines that start with a psql backslash command (e.g. `\d`, `\c`,
/// `\set`). These aren't SQL; the tokenizer would either choke or
/// silently accept garbage.
fn strip_psql_metacommands(input: &str) -> String {
    input
        .lines()
        .filter(|line| !line.trim_start().starts_with('\\'))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Split a SQL script into top-level statements on `;`. Mirrors
/// `src/lint.rs::split_statements` but trimmed: we only need the text,
/// not line numbers.
fn split_statements(input: &str) -> Result<Vec<String>, String> {
    let dialect = PostgreSqlDialect {};
    let tokens = Tokenizer::new(&dialect, input)
        .tokenize_with_location()
        .map_err(|e| e.to_string())?;

    let offsets = line_byte_offsets(input);
    let mut results = Vec::new();
    let mut start_byte: Option<usize> = None;
    let mut end_byte: usize = 0;

    for twl in &tokens {
        match &twl.token {
            Token::Whitespace(_) => {}
            Token::SemiColon => {
                if let Some(start) = start_byte {
                    let text = &input[start..end_byte];
                    let trimmed = text.trim();
                    if !trimmed.is_empty() {
                        // Keep the trailing `;` for parity with other corpus arrays.
                        results.push(format!("{trimmed};"));
                    }
                }
                start_byte = None;
            }
            _ => {
                let tok_start =
                    loc_to_byte(input, &offsets, twl.span.start.line, twl.span.start.column);
                let tok_end_incl =
                    loc_to_byte(input, &offsets, twl.span.end.line, twl.span.end.column);
                let tok_end = input[tok_end_incl..]
                    .chars()
                    .next()
                    .map(|c| tok_end_incl + c.len_utf8())
                    .unwrap_or(input.len());
                if start_byte.is_none() {
                    start_byte = Some(tok_start);
                }
                end_byte = tok_end;
            }
        }
    }
    if let Some(start) = start_byte {
        let text = &input[start..end_byte];
        let trimmed = text.trim();
        if !trimmed.is_empty() {
            results.push(format!("{trimmed};"));
        }
    }
    Ok(results)
}

fn line_byte_offsets(input: &str) -> Vec<usize> {
    let mut offsets = vec![0];
    for (i, b) in input.bytes().enumerate() {
        if b == b'\n' {
            offsets.push(i + 1);
        }
    }
    offsets
}

fn loc_to_byte(input: &str, offsets: &[usize], line: u64, col: u64) -> usize {
    let line_idx = (line as usize).saturating_sub(1);
    let line_start = offsets.get(line_idx).copied().unwrap_or(0);
    let col_chars = (col as usize).saturating_sub(1);
    input[line_start..]
        .char_indices()
        .nth(col_chars)
        .map(|(byte_off, _)| line_start + byte_off)
        .unwrap_or(input.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loads_some_statements() {
        let stmts = statements();
        assert!(
            stmts.len() > 100,
            "expected > 100 statements, got {}",
            stmts.len()
        );
    }

    #[test]
    fn strips_psql_metacommands() {
        let cleaned = strip_psql_metacommands("\\d foo\nSELECT 1;\n\\c other\nSELECT 2;");
        assert_eq!(cleaned, "SELECT 1;\nSELECT 2;");
    }

    #[test]
    fn splits_simple_statements() {
        let stmts = split_statements("CREATE TABLE t (id INT); CREATE INDEX i ON t(id);").unwrap();
        assert_eq!(stmts.len(), 2);
        assert!(stmts[0].starts_with("CREATE TABLE"));
        assert!(stmts[1].starts_with("CREATE INDEX"));
    }

    #[test]
    fn handles_quoted_semicolons() {
        // The tokenizer treats `;` inside string literals as part of the literal.
        let stmts = split_statements("INSERT INTO t VALUES ('a;b'); SELECT 1;").unwrap();
        assert_eq!(stmts.len(), 2);
        assert!(stmts[0].contains("'a;b'"));
    }
}
