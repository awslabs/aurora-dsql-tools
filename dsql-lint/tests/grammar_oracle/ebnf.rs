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
        let body = read_alternation(&mut chars, &mut line)?;
        skip_ws_and_comments(&mut chars, &mut line);
        expect_char(&mut chars, ';', line)?;
        productions.insert(name, body);
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

fn read_alternation<I: Iterator<Item = char>>(
    chars: &mut std::iter::Peekable<I>,
    line: &mut usize,
) -> Result<Production, ParseError> {
    let mut alternatives = vec![read_sequence(chars, line)?];
    loop {
        skip_ws_and_comments(chars, line);
        if chars.peek() == Some(&'|') {
            chars.next();
            skip_ws_and_comments(chars, line);
            alternatives.push(read_sequence(chars, line)?);
        } else {
            break;
        }
    }
    Ok(if alternatives.len() == 1 {
        alternatives.into_iter().next().unwrap()
    } else {
        Production::Choice(alternatives)
    })
}

fn is_terminator(c: char) -> bool {
    matches!(c, ';' | '|' | ']' | '}' | ')')
}

fn read_sequence<I: Iterator<Item = char>>(
    chars: &mut std::iter::Peekable<I>,
    line: &mut usize,
) -> Result<Production, ParseError> {
    // Empty body (e.g. `OptTemp =  ;`) → empty sequence.
    if matches!(chars.peek(), Some(&c) if is_terminator(c)) || chars.peek().is_none() {
        return Ok(Production::Sequence(vec![]));
    }
    let mut atoms = vec![read_atom(chars, line)?];
    loop {
        skip_ws_and_comments(chars, line);
        match chars.peek() {
            Some(&c) if is_terminator(c) => break,
            None => break,
            _ => atoms.push(read_atom(chars, line)?),
        }
    }
    Ok(if atoms.len() == 1 {
        atoms.into_iter().next().unwrap()
    } else {
        Production::Sequence(atoms)
    })
}

fn read_atom<I: Iterator<Item = char>>(
    chars: &mut std::iter::Peekable<I>,
    line: &mut usize,
) -> Result<Production, ParseError> {
    let line_snapshot = *line;
    match chars.peek() {
        Some(&'\'') => Ok(Production::Terminal(read_terminal(chars, line_snapshot)?)),
        Some(&'[') => {
            chars.next();
            skip_ws_and_comments(chars, line);
            let inner = read_alternation(chars, line)?;
            skip_ws_and_comments(chars, line);
            expect_char(chars, ']', *line)?;
            Ok(Production::Optional(Box::new(inner)))
        }
        Some(&'{') => {
            chars.next();
            skip_ws_and_comments(chars, line);
            let inner = read_alternation(chars, line)?;
            skip_ws_and_comments(chars, line);
            expect_char(chars, '}', *line)?;
            Ok(Production::Repetition(Box::new(inner)))
        }
        Some(&'(') => {
            chars.next();
            skip_ws_and_comments(chars, line);
            let inner = read_alternation(chars, line)?;
            skip_ws_and_comments(chars, line);
            expect_char(chars, ')', *line)?;
            Ok(inner)
        }
        Some(&c) if c.is_ascii_alphabetic() || c == '_' => {
            let name = read_identifier(chars).expect("peek confirmed identifier");
            Ok(Production::NonTerminal(name))
        }
        Some(&c) => Err(ParseError {
            line: line_snapshot,
            message: format!("unexpected character '{c}' in production body"),
        }),
        None => Err(ParseError {
            line: line_snapshot,
            message: "unexpected EOF in production body".into(),
        }),
    }
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

    #[test]
    fn parse_non_terminal_reference() {
        let input = "A = B ;";
        let g = parse_grammar(input).expect("parse");
        assert_eq!(
            g.productions.get("A"),
            Some(&Production::NonTerminal("B".into()))
        );
    }

    #[test]
    fn parse_sequence() {
        let input = "Greeting = 'hello' Name ;";
        let g = parse_grammar(input).expect("parse");
        let expected = Production::Sequence(vec![
            Production::Terminal("hello".into()),
            Production::NonTerminal("Name".into()),
        ]);
        assert_eq!(g.productions.get("Greeting"), Some(&expected));
    }

    #[test]
    fn parse_choice() {
        let input = "Bool = 'true' | 'false' ;";
        let g = parse_grammar(input).expect("parse");
        let expected = Production::Choice(vec![
            Production::Terminal("true".into()),
            Production::Terminal("false".into()),
        ]);
        assert_eq!(g.productions.get("Bool"), Some(&expected));
    }

    #[test]
    fn parse_choice_with_sequences() {
        let input = "X = 'a' 'b' | 'c' 'd' ;";
        let g = parse_grammar(input).expect("parse");
        let expected = Production::Choice(vec![
            Production::Sequence(vec![
                Production::Terminal("a".into()),
                Production::Terminal("b".into()),
            ]),
            Production::Sequence(vec![
                Production::Terminal("c".into()),
                Production::Terminal("d".into()),
            ]),
        ]);
        assert_eq!(g.productions.get("X"), Some(&expected));
    }

    #[test]
    fn parse_optional() {
        let input = "X = [ 'a' ] ;";
        let g = parse_grammar(input).expect("parse");
        assert_eq!(
            g.productions.get("X"),
            Some(&Production::Optional(Box::new(Production::Terminal("a".into()))))
        );
    }

    #[test]
    fn parse_repetition() {
        let input = "X = { 'a' } ;";
        let g = parse_grammar(input).expect("parse");
        assert_eq!(
            g.productions.get("X"),
            Some(&Production::Repetition(Box::new(Production::Terminal("a".into()))))
        );
    }

    #[test]
    fn parse_grouping() {
        let input = "X = ( 'a' | 'b' ) 'c' ;";
        let g = parse_grammar(input).expect("parse");
        let expected = Production::Sequence(vec![
            Production::Choice(vec![
                Production::Terminal("a".into()),
                Production::Terminal("b".into()),
            ]),
            Production::Terminal("c".into()),
        ]);
        assert_eq!(g.productions.get("X"), Some(&expected));
    }
}
