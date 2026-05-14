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
pub fn parse_header(_input: &str) -> Result<(FixtureHeader, usize), ParseError> {
    todo!("implement in Task 1.2+")
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert_eq!(&input[body_offset..], "CREATE TABLE t (id BIGINT PRIMARY KEY);\n");
    }
}
