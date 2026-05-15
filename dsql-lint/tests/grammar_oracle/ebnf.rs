//! EBNF text → typed `Grammar` AST.
//!
//! Hand-written parser; the EBNF format is small and regular. We keep this
//! independent of the recognizer (Phase 2+) so changes to the recognizer
//! don't churn the AST and vice versa.

use std::collections::BTreeMap;

#[derive(Debug, PartialEq, Eq, Clone)]
pub struct Grammar {
    pub productions: BTreeMap<String, Production>,
}

#[derive(Debug, PartialEq, Eq, Clone)]
pub enum Production {
    /// `'CREATE'`, `'TABLE'`, etc.
    Terminal(String),
    /// Reference to another production.
    NonTerminal(String),
    /// `A B C` — match in order.
    Sequence(Vec<Production>),
    /// `A | B | C` — match any one.
    Choice(Vec<Production>),
    /// `[ A ]` — zero or one.
    Optional(Box<Production>),
    /// `{ A }` — zero or more.
    Repetition(Box<Production>),
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

pub fn parse_grammar(input: &str) -> Result<Grammar, ParseError> {
    let mut productions = BTreeMap::new();
    let mut line = 1usize;
    let mut chars = input.chars().peekable();

    loop {
        skip_ws_and_comments(&mut chars, &mut line);
        if chars.peek().is_none() {
            break;
        }
        let name = read_identifier(&mut chars).ok_or(ParseError {
            line,
            message: "expected production name".into(),
        })?;
        skip_ws_and_comments(&mut chars, &mut line);
        expect_char(&mut chars, '=', line)?;
        skip_ws_and_comments(&mut chars, &mut line);
        let body = read_terminal(&mut chars, line)?;
        skip_ws_and_comments(&mut chars, &mut line);
        expect_char(&mut chars, ';', line)?;
        productions.insert(name, Production::Terminal(body));
    }

    Ok(Grammar { productions })
}

fn skip_ws_and_comments<I: Iterator<Item = char>>(
    chars: &mut std::iter::Peekable<I>,
    line: &mut usize,
) {
    while let Some(&c) = chars.peek() {
        if c == '\n' {
            *line += 1;
            chars.next();
        } else if c.is_whitespace() {
            chars.next();
        } else {
            break;
        }
    }
}

fn read_identifier<I: Iterator<Item = char>>(
    chars: &mut std::iter::Peekable<I>,
) -> Option<String> {
    let mut s = String::new();
    while let Some(&c) = chars.peek() {
        if c.is_ascii_alphanumeric() || c == '_' {
            s.push(c);
            chars.next();
        } else {
            break;
        }
    }
    if s.is_empty() { None } else { Some(s) }
}

fn read_terminal<I: Iterator<Item = char>>(
    chars: &mut std::iter::Peekable<I>,
    line: usize,
) -> Result<String, ParseError> {
    expect_char(chars, '\'', line)?;
    let mut s = String::new();
    while let Some(&c) = chars.peek() {
        if c == '\'' {
            chars.next();
            return Ok(s);
        }
        s.push(c);
        chars.next();
    }
    Err(ParseError {
        line,
        message: "unterminated quoted terminal".into(),
    })
}

fn expect_char<I: Iterator<Item = char>>(
    chars: &mut std::iter::Peekable<I>,
    expected: char,
    line: usize,
) -> Result<(), ParseError> {
    match chars.next() {
        Some(c) if c == expected => Ok(()),
        Some(c) => Err(ParseError {
            line,
            message: format!("expected '{expected}', got '{c}'"),
        }),
        None => Err(ParseError {
            line,
            message: format!("expected '{expected}', got EOF"),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_single_terminal_production() {
        let input = "Greeting = 'hello' ;";
        let g = parse_grammar(input).expect("parse");
        assert_eq!(g.productions.len(), 1);
        assert_eq!(
            g.productions.get("Greeting"),
            Some(&Production::Terminal("hello".into()))
        );
    }
}
