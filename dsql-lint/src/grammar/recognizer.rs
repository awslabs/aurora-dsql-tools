//! Lowers a `GrammarFile` to an `earlgrey::EarleyParser`. Per-root: each
//! statement-shaped rule needs its own parser.

use earlgrey::{EarleyParser, GrammarBuilder};

use super::model::{GrammarFile, GrammarToken, TokenType};
use super::tokenize::Terminal;

/// CharClass names share a namespace with rule names in the JSON. The
/// `__CC_` prefix keeps the two from colliding when registered with earlgrey.
fn charclass_terminal_name(class: &str) -> String {
    format!("__CC_{class}")
}

fn token_terminal_name(t: &GrammarToken) -> String {
    match t.token_type {
        TokenType::Terminal | TokenType::Quoted => t.text.clone(),
        TokenType::CharClass => charclass_terminal_name(&t.text),
        TokenType::NonTerminal => unreachable!("non-terminal passed to token_terminal_name"),
    }
}

pub fn terminal_input_string(t: &Terminal) -> String {
    match t {
        Terminal::Keyword(s) => s.clone(),
        Terminal::Punct(s) => (*s).to_string(),
        Terminal::CharClass(name) => charclass_terminal_name(name),
        Terminal::Skip => {
            panic!("Terminal::Skip should be filtered before reaching the recognizer")
        }
    }
}

pub struct GrammarRecognizer {
    parser: EarleyParser,
}

impl GrammarRecognizer {
    pub fn build(grammar: &GrammarFile, root: &str) -> Result<Self, String> {
        if !grammar.rules.contains_key(root) {
            return Err(format!("root rule '{root}' not defined in grammar"));
        }

        let mut b = GrammarBuilder::default();

        let mut nonterm_names: std::collections::HashSet<String> =
            grammar.rules.keys().cloned().collect();
        let mut terminal_names: std::collections::HashSet<String> =
            std::collections::HashSet::new();
        for prod in grammar.rules.values() {
            for choice in &prod.choices {
                for t in choice {
                    match t.token_type {
                        TokenType::NonTerminal => {
                            nonterm_names.insert(t.text.clone());
                        }
                        _ => {
                            terminal_names.insert(token_terminal_name(t));
                        }
                    }
                }
            }
            if let Some(sep) = &prod.repetition {
                if !sep.is_empty() {
                    terminal_names.insert(sep.clone());
                }
            }
        }
        // Separators must never collide with a rule name, since the
        // "rule wins over terminal" dedup below would silently drop the
        // separator's predicate and the desugared rule would never match.
        // Today's grammar only uses punctuation separators, but a future
        // refresh introducing a word-shaped one (e.g. `AND`) would trip
        // this without warning.
        for prod in grammar.rules.values() {
            if let Some(sep) = &prod.repetition {
                if !sep.is_empty() && nonterm_names.contains(sep) {
                    return Err(format!(
                        "repetition separator '{sep}' collides with a defined rule name"
                    ));
                }
            }
        }
        // A defined rule wins over a coincidentally-same-named terminal.
        for nt in &nonterm_names {
            terminal_names.remove(nt);
        }

        for term in &terminal_names {
            let owned = term.clone();
            b.terminal_try(term, move |s| s == owned);
        }
        // Includes referenced-but-undefined non-terminals; they end up with
        // zero productions and any derivation through them fails — which is
        // the right behavior when the grammar JSON references a rule it
        // doesn't define.
        for nt in &nonterm_names {
            b.nonterm_try(nt);
        }

        let to_rhs_strings = |choice: &[GrammarToken]| -> Vec<String> {
            choice
                .iter()
                .map(|t| match t.token_type {
                    TokenType::NonTerminal => t.text.clone(),
                    _ => token_terminal_name(t),
                })
                .collect()
        };

        // Desugar each production to plain BNF. Repetition becomes
        // left-recursion plus a base case; optional/zero-or-more adds an
        // empty alternative.
        for (name, prod) in &grammar.rules {
            match (&prod.repetition, prod.optional) {
                (None, false) => {
                    for choice in &prod.choices {
                        let rhs = to_rhs_strings(choice);
                        let refs: Vec<&str> = rhs.iter().map(|s| s.as_str()).collect();
                        b.rule_try(name, &refs);
                    }
                }
                (None, true) => {
                    for choice in &prod.choices {
                        let rhs = to_rhs_strings(choice);
                        let refs: Vec<&str> = rhs.iter().map(|s| s.as_str()).collect();
                        b.rule_try(name, &refs);
                    }
                    b.rule_try(name, &[]);
                }
                (Some(sep), opt) if sep.is_empty() => {
                    for choice in &prod.choices {
                        let rhs = to_rhs_strings(choice);
                        let mut rec = vec![name.clone()];
                        rec.extend(rhs.iter().cloned());
                        let rec_refs: Vec<&str> = rec.iter().map(|s| s.as_str()).collect();
                        b.rule_try(name, &rec_refs);
                        let refs: Vec<&str> = rhs.iter().map(|s| s.as_str()).collect();
                        b.rule_try(name, &refs);
                    }
                    if opt {
                        b.rule_try(name, &[]);
                    }
                }
                (Some(sep), opt) => {
                    for choice in &prod.choices {
                        let rhs = to_rhs_strings(choice);
                        let mut rec = vec![name.clone(), sep.clone()];
                        rec.extend(rhs.iter().cloned());
                        let rec_refs: Vec<&str> = rec.iter().map(|s| s.as_str()).collect();
                        b.rule_try(name, &rec_refs);
                        let refs: Vec<&str> = rhs.iter().map(|s| s.as_str()).collect();
                        b.rule_try(name, &refs);
                    }
                    if opt {
                        b.rule_try(name, &[]);
                    }
                }
            }
        }

        let g = b
            .into_grammar(root)
            .map_err(|e| format!("earlgrey grammar build failed: {e}"))?;
        Ok(GrammarRecognizer {
            parser: EarleyParser::new(g),
        })
    }

    pub fn accepts(&self, tokens: &[String]) -> bool {
        self.parser.parse(tokens.iter().map(|s| s.as_str())).is_ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grammar::model::{GrammarFile, GrammarToken, Production, TokenType};
    use indexmap::IndexMap;

    fn term(text: &str) -> GrammarToken {
        GrammarToken {
            text: text.to_string(),
            token_type: TokenType::Terminal,
        }
    }
    fn nt(text: &str) -> GrammarToken {
        GrammarToken {
            text: text.to_string(),
            token_type: TokenType::NonTerminal,
        }
    }
    fn quoted(text: &str) -> GrammarToken {
        GrammarToken {
            text: text.to_string(),
            token_type: TokenType::Quoted,
        }
    }

    fn build(rules: Vec<(&str, Production)>, root: &str) -> GrammarRecognizer {
        let mut map: IndexMap<String, Production> = IndexMap::new();
        for (n, p) in rules {
            map.insert(n.to_string(), p);
        }
        GrammarRecognizer::build(
            &GrammarFile {
                rules: map,
                root: root.to_string(),
            },
            root,
        )
        .unwrap()
    }

    #[test]
    fn build_fails_when_root_is_undefined() {
        let mut map: IndexMap<String, Production> = IndexMap::new();
        map.insert(
            "Defined".to_string(),
            Production {
                choices: vec![vec![term("x")]],
                optional: false,
                repetition: None,
            },
        );
        let file = GrammarFile {
            rules: map,
            root: "Missing".to_string(),
        };
        let err = GrammarRecognizer::build(&file, "Missing")
            .err()
            .expect("build should fail for undefined root");
        assert!(err.contains("'Missing'"), "unexpected error: {err}");
    }

    #[test]
    fn build_fails_when_separator_collides_with_rule_name() {
        let mut map: IndexMap<String, Production> = IndexMap::new();
        map.insert(
            "AND".to_string(),
            Production {
                choices: vec![vec![term("y")]],
                optional: false,
                repetition: None,
            },
        );
        map.insert(
            "List".to_string(),
            Production {
                choices: vec![vec![term("x")]],
                optional: false,
                repetition: Some("AND".to_string()),
            },
        );
        let file = GrammarFile {
            rules: map,
            root: "List".to_string(),
        };
        let err = GrammarRecognizer::build(&file, "List")
            .err()
            .expect("build should fail when separator collides with rule name");
        assert!(err.contains("'AND'"), "unexpected error: {err}");
    }

    fn toks(ts: &[&str]) -> Vec<String> {
        ts.iter().map(|s| s.to_string()).collect()
    }

    /// Edge case 1: indirect left recursion.
    /// A -> B x ; B -> A y | z.   Strings of form: z x (y x)*.
    #[test]
    fn edge_indirect_left_recursion() {
        let r = build(
            vec![
                (
                    "A",
                    Production {
                        choices: vec![vec![nt("B"), term("x")]],
                        optional: false,
                        repetition: None,
                    },
                ),
                (
                    "B",
                    Production {
                        choices: vec![vec![nt("A"), term("y")], vec![term("z")]],
                        optional: false,
                        repetition: None,
                    },
                ),
            ],
            "A",
        );

        assert!(r.accepts(&toks(&["z", "x"])));
        assert!(r.accepts(&toks(&["z", "x", "y", "x"])));
        assert!(r.accepts(&toks(&["z", "x", "y", "x", "y", "x"])));
        assert!(!r.accepts(&toks(&["z"])));
        assert!(!r.accepts(&toks(&["x"])));
        assert!(!r.accepts(&toks(&["z", "x", "y"])));
    }

    /// Edge case 2: empty production. A -> ε.
    #[test]
    fn edge_empty_production() {
        let r = build(
            vec![(
                "A",
                Production {
                    choices: vec![vec![]],
                    optional: false,
                    repetition: None,
                },
            )],
            "A",
        );
        assert!(r.accepts(&toks(&[])));
        // (A non-empty input fails because there's no rule that consumes a
        // token. We don't bother to register a stray terminal here; the
        // empty-input case is the load-bearing one.)
    }

    /// Edge case 3: comma-separated repetition.
    /// List -> x { , x }. Implemented via the repetition desugaring.
    #[test]
    fn edge_comma_repetition() {
        let r = build(
            vec![(
                "List",
                Production {
                    choices: vec![vec![term("x")]],
                    optional: false,
                    repetition: Some(",".to_string()),
                },
            )],
            "List",
        );
        assert!(r.accepts(&toks(&["x"])));
        assert!(r.accepts(&toks(&["x", ",", "x"])));
        assert!(r.accepts(&toks(&["x", ",", "x", ",", "x"])));
        assert!(!r.accepts(&toks(&[","])));
        assert!(!r.accepts(&toks(&["x", ","])));
        assert!(!r.accepts(&toks(&[",", "x"])));
    }

    /// Sanity: small SQL-shaped grammar mixing Quoted punctuation, terminals,
    /// and a comma-separated list.
    #[test]
    fn small_sql_shape() {
        let r = build(
            vec![
                (
                    "Stmt",
                    Production {
                        choices: vec![vec![
                            term("CREATE"),
                            term("TABLE"),
                            nt("Name"),
                            quoted("("),
                            nt("Cols"),
                            quoted(")"),
                        ]],
                        optional: false,
                        repetition: None,
                    },
                ),
                (
                    "Name",
                    Production {
                        choices: vec![vec![term("IDENT")]],
                        optional: false,
                        repetition: None,
                    },
                ),
                (
                    "Cols",
                    Production {
                        choices: vec![vec![nt("Col")]],
                        optional: false,
                        repetition: Some(",".to_string()),
                    },
                ),
                (
                    "Col",
                    Production {
                        choices: vec![
                            vec![nt("Name"), term("INT")],
                            vec![nt("Name"), term("TEXT")],
                        ],
                        optional: false,
                        repetition: None,
                    },
                ),
            ],
            "Stmt",
        );

        assert!(r.accepts(&toks(&[
            "CREATE", "TABLE", "IDENT", "(", "IDENT", "INT", ")"
        ])));
        assert!(r.accepts(&toks(&[
            "CREATE", "TABLE", "IDENT", "(", "IDENT", "INT", ",", "IDENT", "TEXT", ")"
        ])));
        assert!(!r.accepts(&toks(&[
            "CRATE", "TABLE", "IDENT", "(", "IDENT", "INT", ")"
        ])));
        assert!(!r.accepts(&toks(&["CREATE", "TABLE", "IDENT", "(", "IDENT", ",", ")"])));
    }
}
