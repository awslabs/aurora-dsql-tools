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
