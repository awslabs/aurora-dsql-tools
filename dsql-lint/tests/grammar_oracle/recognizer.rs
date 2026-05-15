//! Build a chumsky recognizer from a `Grammar` AST.
//!
//! The recognizer's only job is `accepts(sql) -> bool` — yes/no for the
//! drift oracle. No structured output, no error reporting.

use crate::grammar_oracle::ebnf::Grammar;

pub struct Recognizer {
    grammar: Grammar,
    start: String,
}

impl Recognizer {
    pub fn build(grammar: Grammar, start: &str) -> Self {
        Self {
            grammar,
            start: start.to_string(),
        }
    }

    pub fn accepts(&self, _sql: &str) -> bool {
        todo!("Task 2.2")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grammar_oracle::ebnf::parse_grammar;

    #[test]
    fn recognizer_accepts_single_terminal() {
        let g = parse_grammar("Greeting = 'hello' ;").unwrap();
        let r = Recognizer::build(g, "Greeting");
        assert!(r.accepts("hello"));
        assert!(!r.accepts("world"));
    }
}
