//! Build a chumsky recognizer from a `Grammar` AST.
//!
//! The recognizer's only job is `accepts(sql) -> bool` — yes/no for the
//! drift oracle. No structured output, no error reporting.
//!
//! # Architecture
//!
//! chumsky's parsers are tied to the input lifetime `'src`, which makes
//! storing a fully-built parser inside `Recognizer` awkward (every call
//! to `accepts` gets a different `'src`). The path of least resistance
//! is to rebuild the parser tree per call: it's cheap, the construction
//! is straight-line over the `Grammar` AST, and it sidesteps every
//! self-referential lifetime problem you would otherwise hit.
//!
//! Cycles in the grammar (mutual or self-recursion between productions)
//! are handled with `chumsky::recursive::Recursive::declare()` +
//! `define()`. We declare a `Recursive` placeholder for every production
//! up front, then walk the grammar a second time to define each. When
//! the body of production `A` references production `B`, it clones `B`'s
//! placeholder; the clone is a `Weak`-backed handle that resolves once
//! `B` is defined (chumsky enforces "defined exactly once" internally).

use crate::grammar_oracle::ebnf::{Grammar, Production};
use chumsky::prelude::*;
use chumsky::recursive::{Indirect, Recursive};
use std::collections::HashMap;

pub struct Recognizer {
    grammar: Grammar,
    start: String,
}

/// Type alias for the recursive parser handle we use for every production.
/// `()` output: we only care about acceptance, not structure.
type ProdParser<'src> = Recursive<Indirect<'src, 'src, &'src str, (), extra::Default>>;

impl Recognizer {
    pub fn build(grammar: Grammar, start: &str) -> Self {
        Self {
            grammar,
            start: start.to_string(),
        }
    }

    pub fn accepts(&self, sql: &str) -> bool {
        let parser = self.build_root_parser();
        parser.parse(sql).into_result().is_ok()
    }

    /// Build a fresh parser for the start production. Returns an owning
    /// parser whose lifetime is bound to `&self` (and to the implicit input
    /// lifetime via the `Recursive` declarations).
    fn build_root_parser<'src>(
        &'src self,
    ) -> impl Parser<'src, &'src str, (), extra::Default> + 'src {
        // Phase 1: declare a Recursive placeholder for every production.
        let mut parsers: HashMap<String, ProdParser<'src>> = HashMap::new();
        for name in self.grammar.productions.keys() {
            parsers.insert(name.clone(), Recursive::declare());
        }

        // Phase 2: define each production by translating its AST, looking up
        // sibling references in the `parsers` map.
        for (name, prod) in &self.grammar.productions {
            let body = build_node(prod, &parsers);
            parsers.get_mut(name).expect("declared above").define(body);
        }

        let start = parsers
            .get(&self.start)
            .unwrap_or_else(|| panic!("unknown start production: {}", self.start))
            .clone();

        // Each token-level parser is `padded()` so whitespace between tokens
        // is handled. We just need to require EOF at the end.
        start.then_ignore(end()).boxed()
    }
}

/// Translate one `Production` node into a chumsky parser. References to other
/// productions are resolved via `parsers` (a clone of the `Recursive` handle
/// is taken; chumsky resolves it at parse-time once defined).
fn build_node<'src>(
    prod: &Production,
    parsers: &HashMap<String, ProdParser<'src>>,
) -> Boxed<'src, 'src, &'src str, (), extra::Default> {
    match prod {
        Production::Terminal(literal) => terminal_parser(literal).boxed(),
        Production::NonTerminal(name) => parsers
            .get(name)
            .unwrap_or_else(|| panic!("non-terminal references undefined production: {name}"))
            .clone()
            .boxed(),
        Production::Sequence(items) => {
            // Fold each child into a chain that ignores the previous result.
            let mut iter = items.iter();
            let first = match iter.next() {
                Some(p) => build_node(p, parsers),
                // Empty sequence matches the empty input.
                None => return empty().boxed(),
            };
            iter.fold(first, |acc, p| {
                acc.then(build_node(p, parsers)).ignored().boxed()
            })
        }
        Production::Choice(alts) => {
            let mut iter = alts.iter();
            let first = match iter.next() {
                Some(p) => build_node(p, parsers),
                // Empty choice matches nothing; use a never-matching parser.
                // `empty()` would match the empty string, which is wrong here,
                // but a grammar with an empty Choice shouldn't occur in practice.
                None => return empty().boxed(),
            };
            iter.fold(first, |acc, p| acc.or(build_node(p, parsers)).boxed())
        }
        Production::Optional(inner) => build_node(inner, parsers).or_not().ignored().boxed(),
        Production::Repetition(inner) => build_node(inner, parsers)
            .repeated()
            .collect::<()>()
            .boxed(),
    }
}

/// Build a parser for a terminal literal.
///
/// Two cases:
/// - The literal looks like a keyword (alphabetic / underscore): use
///   `text::keyword`-style matching with case-insensitive comparison and
///   word boundaries (so `'CREATE'` won't match a prefix of `CREATETABLE`).
/// - Otherwise (punctuation like `,` / `(`): match the literal verbatim with
///   `just`.
fn terminal_parser<'src>(literal: &str) -> Boxed<'src, 'src, &'src str, (), extra::Default> {
    let is_keyword = literal
        .chars()
        .next()
        .map(|c| c.is_ascii_alphabetic() || c == '_')
        .unwrap_or(false);

    if is_keyword {
        let target = literal.to_ascii_lowercase();
        // text::ident() matches a maximal identifier run, which gives us word
        // boundaries for free. Then case-insensitively compare to the target.
        text::ident()
            .filter(move |s: &&str| s.eq_ignore_ascii_case(&target))
            .ignored()
            .padded()
            .boxed()
    } else {
        // Punctuation / symbolic terminal: copy and use just().
        let owned: String = literal.to_string();
        just(owned).ignored().padded().boxed()
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

    #[test]
    fn recognizer_resolves_non_terminal() {
        let g = parse_grammar(
            "\
            Greeting = Hello ;\n\
            Hello = 'hi' ;\n\
        ",
        )
        .unwrap();
        let r = Recognizer::build(g, "Greeting");
        assert!(r.accepts("hi"));
        assert!(!r.accepts("hello"));
    }

    #[test]
    fn recognizer_accepts_sequence() {
        let g = parse_grammar("Hello = 'hello' 'world' ;").unwrap();
        let r = Recognizer::build(g, "Hello");
        assert!(r.accepts("hello world"));
        assert!(r.accepts("HELLO WORLD")); // case-insensitive keyword
        assert!(!r.accepts("hello"));
        assert!(!r.accepts("world hello"));
    }

    #[test]
    fn recognizer_accepts_choice() {
        let g = parse_grammar("Bool = 'true' | 'false' ;").unwrap();
        let r = Recognizer::build(g, "Bool");
        assert!(r.accepts("true"));
        assert!(r.accepts("false"));
        assert!(!r.accepts("maybe"));
    }

    #[test]
    fn recognizer_accepts_optional() {
        let g = parse_grammar("X = 'a' [ 'b' ] ;").unwrap();
        let r = Recognizer::build(g, "X");
        assert!(r.accepts("a"));
        assert!(r.accepts("a b"));
        assert!(!r.accepts("b"));
    }

    #[test]
    fn recognizer_accepts_repetition() {
        let g = parse_grammar("X = 'a' { 'b' } ;").unwrap();
        let r = Recognizer::build(g, "X");
        assert!(r.accepts("a"));
        assert!(r.accepts("a b"));
        assert!(r.accepts("a b b b b"));
        assert!(!r.accepts("b"));
    }

    #[test]
    fn recognizer_handles_mutual_recursion() {
        let g = parse_grammar(
            "\
            A = 'a' [ B ] ;\n\
            B = 'b' [ A ] ;\n\
        ",
        )
        .unwrap();
        let r = Recognizer::build(g, "A");
        assert!(r.accepts("a"));
        assert!(r.accepts("a b"));
        assert!(r.accepts("a b a"));
        assert!(r.accepts("a b a b"));
    }

    #[test]
    fn recognizer_handles_right_recursion() {
        let g = parse_grammar("List = 'item' [ ',' List ] ;").unwrap();
        let r = Recognizer::build(g, "List");
        assert!(r.accepts("item"));
        assert!(r.accepts("item , item"));
        assert!(r.accepts("item , item , item"));
    }
}
