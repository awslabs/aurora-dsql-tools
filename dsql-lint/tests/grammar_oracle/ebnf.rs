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

pub fn parse_grammar(_input: &str) -> Result<Grammar, ParseError> {
    todo!("Task 1.2+")
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
