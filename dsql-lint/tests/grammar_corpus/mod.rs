//! Helpers for the grammar corpus oracle (`tests/grammar_oracle.rs`).
//!
//! Owns the fixture header parser, fixture loader, and the EBNF production
//! extractor. Test-only — never compiled into the shipped binary.

#[derive(Debug, PartialEq, Eq)]
pub struct FixtureHeader {
    pub production: String,
    pub expectation: Expectation,
    pub rule: Option<String>,
    pub fix: Option<String>,
    pub fixes: Option<String>,
}

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum Expectation {
    Accept,
    Reject,
}

#[derive(Debug)]
pub struct ParseError {
    pub line: usize,
    pub message: String,
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "line {}: {}", self.line, self.message)
    }
}

/// Parse the leading SQL-comment header. Returns the header and the byte
/// offset where the SQL body begins (i.e. the first non-comment, non-blank line).
pub fn parse_header(input: &str) -> Result<(FixtureHeader, usize), ParseError> {
    let mut production: Option<String> = None;
    let mut expectation: Option<Expectation> = None;
    let mut rule: Option<String> = None;
    let mut fix: Option<String> = None;
    let mut fixes: Option<String> = None;

    let mut byte_offset = 0;
    for (line_idx, line) in input.split_inclusive('\n').enumerate() {
        let trimmed = line.trim_end_matches('\n').trim_end_matches('\r');
        let stripped = trimmed.trim_start();
        if stripped.is_empty() {
            byte_offset += line.len();
            continue;
        }
        if !stripped.starts_with("--") {
            // First non-comment, non-blank line: header ends here.
            break;
        }
        let after_dashes = stripped.trim_start_matches('-').trim_start();
        // Tolerate full-line comments without `key:` (e.g. licence banners).
        if let Some((key, value)) = after_dashes.split_once(':') {
            let key = key.trim();
            let value = value.trim().to_string();
            let line_no = line_idx + 1;
            match key {
                "production" => production = Some(value),
                "expectation" => {
                    expectation = Some(match value.as_str() {
                        "accept" => Expectation::Accept,
                        "reject" => Expectation::Reject,
                        other => {
                            return Err(ParseError {
                                line: line_no,
                                message: format!(
                                    "expectation must be 'accept' or 'reject', got '{other}'"
                                ),
                            })
                        }
                    })
                }
                "rule" => rule = Some(value),
                "fix" => fix = Some(value),
                "fixes" => fixes = Some(value),
                other => {
                    return Err(ParseError {
                        line: line_no,
                        message: format!("unknown header key '{other}'"),
                    })
                }
            }
        }
        byte_offset += line.len();
    }

    let production = production.ok_or(ParseError {
        line: 1,
        message: "missing required key 'production'".into(),
    })?;
    let expectation = expectation.ok_or(ParseError {
        line: 1,
        message: "missing required key 'expectation'".into(),
    })?;

    Ok((
        FixtureHeader {
            production,
            expectation,
            rule,
            fix,
            fixes,
        },
        byte_offset,
    ))
}

use std::path::{Path, PathBuf};

#[derive(Debug)]
pub struct Fixture {
    /// Path relative to `tests/grammar/`, e.g. "accept/serial_type__basic.sql".
    pub rel_path: String,
    pub kind: FixtureKind,
    pub header: FixtureHeader,
    pub body: String,
}

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum FixtureKind {
    Accept,
    Reject,
    Fixed,
}

/// Walk `<crate>/tests/grammar/{accept,reject,fixed}/` and load every `*.sql`
/// fixture. Panics on the first malformed fixture so test failures point at
/// the offending file directly.
pub fn load_corpus() -> Vec<Fixture> {
    let root = corpus_root();
    let mut out = Vec::new();
    for (sub, kind) in [
        ("accept", FixtureKind::Accept),
        ("reject", FixtureKind::Reject),
        ("fixed", FixtureKind::Fixed),
    ] {
        let dir = root.join(sub);
        if !dir.exists() {
            continue;
        }
        for entry in
            std::fs::read_dir(&dir).unwrap_or_else(|e| panic!("read_dir {}: {e}", dir.display()))
        {
            let entry = entry.expect("dir entry");
            let path = entry.path();
            // Skip subdirectories (e.g. `_coverage_gap/`) and non-SQL files.
            // Subdirectories are excluded implicitly because their paths have
            // no `.sql` extension; the convention is that any subdirectory
            // (especially one prefixed with `_`) is invisible to the loader.
            if path.extension().and_then(|e| e.to_str()) != Some("sql") {
                continue;
            }
            let contents = std::fs::read_to_string(&path)
                .unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
            let (header, body_offset) = parse_header(&contents)
                .unwrap_or_else(|e| panic!("malformed header in {}: {e}", path.display()));
            // Header expectation must agree with directory.
            let expected = match kind {
                FixtureKind::Accept | FixtureKind::Fixed => Expectation::Accept,
                FixtureKind::Reject => Expectation::Reject,
            };
            assert_eq!(
                header.expectation,
                expected,
                "{}: header expectation does not match directory {sub}/",
                path.display()
            );
            let body = contents[body_offset..].to_string();
            let rel_path = format!("{sub}/{}", path.file_name().unwrap().to_string_lossy());
            out.push(Fixture {
                rel_path,
                kind,
                header,
                body,
            });
        }
    }
    out
}

pub fn corpus_root() -> PathBuf {
    // CARGO_MANIFEST_DIR points at dsql-lint/.
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("grammar")
}

/// Extract production names from an EBNF grammar text.
///
/// Productions look like `Foo = ... ;` — match identifiers at line start
/// followed by `=`. Tolerates whitespace before the `=`.
pub fn extract_production_names(ebnf: &str) -> Vec<String> {
    let re = regex::Regex::new(r"(?m)^([A-Za-z][A-Za-z0-9_]*)\s*=").unwrap();
    re.captures_iter(ebnf).map(|c| c[1].to_string()).collect()
}

/// Convert `snake_case` (the on-wire form of `LintRule` via serde) to
/// `PascalCase` (the variant identifier as printed by `{:?}`).
pub fn snake_to_pascal(s: &str) -> String {
    s.split('_')
        .map(|seg| {
            let mut c = seg.chars();
            match c.next() {
                Some(first) => first.to_uppercase().chain(c).collect::<String>(),
                None => String::new(),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snake_to_pascal_basic() {
        assert_eq!(snake_to_pascal("serial_type"), "SerialType");
        assert_eq!(snake_to_pascal("foreign_key"), "ForeignKey");
        assert_eq!(
            snake_to_pascal("identity_cache_missing"),
            "IdentityCacheMissing"
        );
        assert_eq!(snake_to_pascal(""), "");
        assert_eq!(snake_to_pascal("x"), "X");
    }

    #[test]
    fn parse_header_accept_minimal() {
        let input = "\
-- production: CreateStmt
-- expectation: accept
CREATE TABLE t (id BIGINT PRIMARY KEY);
";
        let (header, body_offset) = parse_header(input).expect("parse should succeed");
        assert_eq!(header.production, "CreateStmt");
        assert_eq!(header.expectation, Expectation::Accept);
        assert_eq!(header.rule, None);
        assert_eq!(header.fix, None);
        assert_eq!(header.fixes, None);
        assert_eq!(
            &input[body_offset..],
            "CREATE TABLE t (id BIGINT PRIMARY KEY);\n"
        );
    }

    #[test]
    fn parse_header_reject_with_fix() {
        let input = "\
-- production: CreateStmt
-- expectation: reject
-- rule: serial_type
-- fix: fixed/serial_type__basic.sql
CREATE TABLE t (id SERIAL);
";
        let (h, _) = parse_header(input).unwrap();
        assert_eq!(h.expectation, Expectation::Reject);
        assert_eq!(h.rule.as_deref(), Some("serial_type"));
        assert_eq!(h.fix.as_deref(), Some("fixed/serial_type__basic.sql"));
    }

    #[test]
    fn parse_header_fixed_with_back_reference() {
        let input = "\
-- production: CreateStmt
-- expectation: accept
-- fixes: reject/serial_type__basic.sql
CREATE TABLE t (id BIGINT);
";
        let (h, _) = parse_header(input).unwrap();
        assert_eq!(h.fixes.as_deref(), Some("reject/serial_type__basic.sql"));
    }

    #[test]
    fn parse_header_missing_production_errors() {
        let input = "-- expectation: accept\nSELECT 1;\n";
        let err = parse_header(input).unwrap_err();
        assert!(err.message.contains("production"), "got {err}");
    }

    #[test]
    fn parse_header_unknown_key_errors() {
        let input = "\
-- production: X
-- expectation: accept
-- frobnicate: yes
SELECT 1;
";
        let err = parse_header(input).unwrap_err();
        assert!(err.message.contains("frobnicate"), "got {err}");
    }

    #[test]
    fn parse_header_bad_expectation_errors() {
        let input = "\
-- production: X
-- expectation: maybe
SELECT 1;
";
        let err = parse_header(input).unwrap_err();
        assert!(err.message.contains("expectation"), "got {err}");
    }

    #[test]
    fn parse_header_skips_leading_blank_lines() {
        let input = "\n-- production: X\n-- expectation: accept\nSELECT 1;\n";
        let (h, body_offset) = parse_header(input).unwrap();
        assert_eq!(h.production, "X");
        assert_eq!(&input[body_offset..], "SELECT 1;\n");
    }

    #[test]
    fn extract_production_names_basic() {
        let ebnf = "\
Foo = 'a' ;
Bar = Foo | 'b' ;
-- comment that should be skipped
";
        let prods = super::extract_production_names(ebnf);
        assert!(prods.contains(&"Foo".to_string()));
        assert!(prods.contains(&"Bar".to_string()));
        assert!(!prods.contains(&"comment".to_string()));
    }

    #[test]
    fn load_corpus_empty_is_ok() {
        // Don't depend on actual fixtures yet; just make sure the function
        // doesn't panic when the directory is empty or partial.
        let _ = load_corpus(); // no assertion — just must not panic
    }
}
