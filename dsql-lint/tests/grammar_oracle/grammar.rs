//! Vendored types matching the `dsql_grammar.json` shape, plus an
//! input-driven derivation oracle.
//!
//! ## How it works
//!
//! The grammar is the source of truth. To answer "does the grammar accept
//! this SQL?" we tokenize the SQL and ask: "starting from each `*Stmt`
//! rule, can I find a derivation that consumes the tokens in order?"
//!
//! This is input-driven, not derivation-driven. The previous reachability
//! approach (enumerate all derivations the grammar can produce, check if
//! one matches) blew up exponentially because grammar derivation is
//! unbounded. Input-driven derivation is bounded by the input length:
//! the cursor only moves forward, and we memoize on `(rule, cursor)` so
//! each subproblem is solved once.
//!
//! ## Algorithm
//!
//! `accepts(sql)` tokenizes, then for each statement rule tries to
//! derive: pick any alternative whose token sequence can consume the
//! input from the current cursor. Recurse on nonterminals. Match
//! keywords case-insensitively. Match `CharClass` tokens (`IDENT`,
//! `ICONST`, `SCONST`, etc.) by token category. Memoize on
//! `(rule_name, cursor)` to keep complexity polynomial.
//!
//! Cycle detection: if a rule is already being computed at the same
//! cursor (left recursion without consuming input), return `Failed` for
//! that branch — cycles can't accept new tokens.
//!
//! ## What "accept" means here
//!
//! We accept a statement if SOME `*Stmt` rule has a derivation that
//! consumes ALL the tokens. Trailing semicolons are stripped before
//! tokenizing. Statements that partially-match (consume some prefix but
//! not all) are rejected — same shape as a real parser.

use serde::Deserialize;
use std::collections::{BTreeMap, HashMap, HashSet};

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Production {
    pub choices: Vec<Vec<GrammarToken>>,
    pub optional: bool,
    /// If `Some(sep)`, the rule matches one occurrence of any choice,
    /// optionally followed by `sep` then another occurrence, repeated.
    /// The grammar JSON encodes lists like `expr_list = a_expr [, a_expr]*`
    /// this way: a single-choice production `[[a_expr]]` with
    /// `repetition: ","`.
    pub repetition: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub enum TokenType {
    Terminal,
    NonTerminal,
    Quoted,
    CharClass,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct GrammarToken {
    pub text: String,
    pub token_type: TokenType,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Grammar {
    pub rules: BTreeMap<String, Production>,
    #[allow(dead_code)]
    pub root: String,
}

impl Grammar {
    pub fn load(path: &std::path::Path) -> Self {
        let text = std::fs::read_to_string(path)
            .unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
        serde_json::from_str(&text).unwrap_or_else(|e| panic!("parse {}: {e}", path.display()))
    }

    /// Names of all `*Stmt` rules — the candidate roots for a single SQL
    /// statement. Synthesized because the JSON's `root` field references a
    /// meta-symbol that isn't in `rules`.
    pub fn statement_rules(&self) -> Vec<&str> {
        self.rules
            .keys()
            .filter(|k| {
                let s = k.as_str();
                s.ends_with("Stmt")
                    && !s.contains("__in__")
                    && s != "ExplainableStmt"
                    && s != "PreparableStmt"
            })
            .map(String::as_str)
            .collect()
    }

    /// Top-level entry: does the grammar accept this SQL statement?
    pub fn accepts(&self, sql: &str) -> bool {
        let tokens = tokenize(sql);
        if tokens.is_empty() {
            return false;
        }
        let mut ctx = DerivCtx::new(self, &tokens);
        for stmt_rule in self.statement_rules() {
            if ctx.derive_rule(stmt_rule, 0) == Some(tokens.len()) {
                return true;
            }
        }
        false
    }
}

/// A tokenized input position. Either a raw word/keyword (uppercased so
/// matching is case-insensitive) or a punctuation character. `Number`
/// and `String` are flagged so they match `ICONST`/`FCONST`/`SCONST`
/// CharClass tokens.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Tok {
    Word(String),
    Punct(char),
    Number,
    String,
}

impl Tok {
    fn matches_keyword(&self, kw: &str) -> bool {
        match self {
            Tok::Word(w) => w.eq_ignore_ascii_case(kw),
            Tok::Punct(c) => kw.len() == 1 && kw.starts_with(*c),
            _ => false,
        }
    }

    fn matches_char_class(&self, class: &str) -> bool {
        match (self, class) {
            (Tok::Word(_), "IDENT" | "UIDENT") => true,
            (Tok::Number, "ICONST" | "FCONST") => true,
            (Tok::String, "SCONST" | "USCONST") => true,
            // `Op` is a Postgres operator. Permissive but not universal:
            // operator-class punctuation only. Excludes comma/paren/etc.
            // that are structural grammar tokens.
            (Tok::Punct(c), "Op") => matches!(
                c,
                '+' | '-'
                    | '*'
                    | '/'
                    | '%'
                    | '^'
                    | '<'
                    | '>'
                    | '='
                    | '!'
                    | '~'
                    | '@'
                    | '#'
                    | '&'
                    | '|'
                    | '`'
                    | '?'
            ),
            // Bit/hex string literals: rare; treat as string-shaped tokens.
            (Tok::String, "BCONST" | "XCONST") => true,
            // PARAM (`$N`) — we tokenize this as Punct('$') then a number,
            // which would need a fix-up. For now don't accept any token
            // here; statements using `$N` parameters will reject, which is
            // honest given the tokenizer doesn't emit a PARAM tok yet.
            _ => false,
        }
    }
}

pub fn tokenize(sql: &str) -> Vec<Tok> {
    let mut out = Vec::new();
    // Strip a trailing semicolon — corpus statements include it; we treat
    // statements as already-split.
    let trimmed = sql.trim().trim_end_matches(';').trim_end();
    let mut chars = trimmed.chars().peekable();
    while let Some(&c) = chars.peek() {
        if c.is_whitespace() {
            chars.next();
        } else if c == '-' && {
            let mut peek = chars.clone();
            peek.next();
            peek.peek() == Some(&'-')
        } {
            // line comment
            for c2 in chars.by_ref() {
                if c2 == '\n' {
                    break;
                }
            }
        } else if c.is_ascii_alphabetic() || c == '_' {
            let mut s = String::new();
            while let Some(&cc) = chars.peek() {
                if cc.is_ascii_alphanumeric() || cc == '_' {
                    s.push(cc);
                    chars.next();
                } else {
                    break;
                }
            }
            out.push(Tok::Word(s.to_ascii_uppercase()));
        } else if c.is_ascii_digit() {
            while let Some(&cc) = chars.peek() {
                if cc.is_ascii_digit() || cc == '.' {
                    chars.next();
                } else {
                    break;
                }
            }
            out.push(Tok::Number);
        } else if c == '\'' {
            chars.next();
            while let Some(cc) = chars.next() {
                if cc == '\'' {
                    if chars.peek() == Some(&'\'') {
                        chars.next();
                    } else {
                        break;
                    }
                }
            }
            out.push(Tok::String);
        } else if c == '"' {
            // quoted identifier
            chars.next();
            let mut s = String::new();
            while let Some(cc) = chars.next() {
                if cc == '"' {
                    if chars.peek() == Some(&'"') {
                        s.push('"');
                        chars.next();
                    } else {
                        break;
                    }
                } else {
                    s.push(cc);
                }
            }
            out.push(Tok::Word(s.to_ascii_uppercase()));
        } else {
            chars.next();
            out.push(Tok::Punct(c));
        }
    }
    out
}

/// Memoized derivation engine. One `DerivCtx` per `accepts` call, so the
/// memo table is per-input — different inputs produce different memo keys.
struct DerivCtx<'g, 't> {
    grammar: &'g Grammar,
    tokens: &'t [Tok],
    /// Memo: `(rule_name, cursor)` -> Some(new_cursor) on success, None on
    /// failure. We use a separate `in_progress` set for cycle detection.
    memo: HashMap<(String, usize), Option<usize>>,
    in_progress: HashSet<(String, usize)>,
}

impl<'g, 't> DerivCtx<'g, 't> {
    fn new(grammar: &'g Grammar, tokens: &'t [Tok]) -> Self {
        Self {
            grammar,
            tokens,
            memo: HashMap::new(),
            in_progress: HashSet::new(),
        }
    }

    /// Try to derive `rule_name` starting at `cursor`. Returns the new
    /// cursor on success, or `None` if no derivation works.
    ///
    /// Greedy: returns the LONGEST match found. For `accepts()` to work
    /// correctly we need the longest match because the caller checks
    /// `== tokens.len()`. A shorter successful match would be returned
    /// from this function but rejected by the caller, making the
    /// statement (incorrectly) fail.
    fn derive_rule(&mut self, rule_name: &str, cursor: usize) -> Option<usize> {
        let key = (rule_name.to_string(), cursor);

        // Cycle: rule already on the call stack at this cursor. Return
        // None — recursive expansion can't make progress without consuming
        // a token, and we haven't consumed one yet at this cursor.
        if self.in_progress.contains(&key) {
            return None;
        }
        if let Some(&cached) = self.memo.get(&key) {
            return cached;
        }

        let prod = match self.grammar.rules.get(rule_name) {
            Some(p) => p.clone(),
            None => {
                // Undefined nonterminal (`SignedIconst`, `var_list`, etc).
                // Conservatively consume one token to avoid a false-reject
                // for grammar gaps the grammar itself doesn't fill.
                let result = if cursor < self.tokens.len() {
                    Some(cursor + 1)
                } else {
                    None
                };
                self.memo.insert(key, result);
                return result;
            }
        };

        self.in_progress.insert(key.clone());

        // Partition choices into base (no leading self-reference) and
        // left-recursive (first symbol is a NonTerminal whose name equals
        // this rule). Standard transform: A = A α | β  ≡  A = β α*
        // We match a base β first, then iteratively try each tail α.
        let mut base_choices: Vec<&Vec<GrammarToken>> = Vec::new();
        let mut lrec_tails: Vec<&[GrammarToken]> = Vec::new();
        for choice in &prod.choices {
            let is_lrec = choice
                .first()
                .map(|tok| {
                    matches!(tok.token_type, TokenType::NonTerminal) && tok.text == rule_name
                })
                .unwrap_or(false);
            if is_lrec {
                lrec_tails.push(&choice[1..]);
            } else {
                base_choices.push(choice);
            }
        }

        // Try each base choice; keep the longest end.
        let mut best: Option<usize> = None;
        for choice in &base_choices {
            if let Some(end) = self.match_sequence(choice, cursor) {
                best = match best {
                    Some(b) if b >= end => Some(b),
                    _ => Some(end),
                };
            }
        }

        // Iteratively extend with left-recursive tails. Each iteration
        // tries every tail at the current `best` cursor; if any extends
        // strictly further, take that and loop.
        if let Some(mut cur) = best {
            loop {
                let mut extended: Option<usize> = None;
                for tail in &lrec_tails {
                    if let Some(end) = self.match_sequence(tail, cur) {
                        if end > cur {
                            extended = match extended {
                                Some(e) if e >= end => Some(e),
                                _ => Some(end),
                            };
                        }
                    }
                }
                match extended {
                    Some(e) => {
                        cur = e;
                        best = Some(e);
                    }
                    None => break,
                }
            }
        }

        // Repetition: greedy `(choice sep)+` (or `choice+` if sep is "").
        // Applied to the base choices only — left-recursive rules don't
        // typically combine with explicit repetition in this grammar shape.
        if let Some(sep) = &prod.repetition {
            if let Some(end) = best {
                let mut c = end;
                loop {
                    let after_sep = if sep.is_empty() {
                        c
                    } else {
                        if c >= self.tokens.len() {
                            break;
                        }
                        let sep_first = sep.chars().next().unwrap();
                        let sep_matched = sep.len() == 1
                            && matches!(&self.tokens[c], Tok::Punct(p) if *p == sep_first);
                        let sep_kw_matched = !sep_matched && self.tokens[c].matches_keyword(sep);
                        if !sep_matched && !sep_kw_matched {
                            break;
                        }
                        c + 1
                    };
                    let mut iter_best: Option<usize> = None;
                    for choice in &base_choices {
                        if let Some(e) = self.match_sequence(choice, after_sep) {
                            iter_best = match iter_best {
                                Some(x) if x >= e => Some(x),
                                _ => Some(e),
                            };
                        }
                    }
                    match iter_best {
                        Some(e) if e > c => {
                            c = e;
                            best = Some(c);
                        }
                        _ => break,
                    }
                }
            }
        }

        if prod.optional {
            best = match best {
                Some(b) if b >= cursor => Some(b),
                _ => Some(cursor),
            };
        }

        self.in_progress.remove(&key);
        self.memo.insert(key, best);
        best
    }

    fn match_sequence(&mut self, choice: &[GrammarToken], cursor: usize) -> Option<usize> {
        let mut c = cursor;
        for tok in choice {
            c = self.match_token(tok, c)?;
        }
        Some(c)
    }

    fn match_token(&mut self, tok: &GrammarToken, cursor: usize) -> Option<usize> {
        match tok.token_type {
            TokenType::Terminal | TokenType::Quoted => {
                if cursor < self.tokens.len() && self.tokens[cursor].matches_keyword(&tok.text) {
                    Some(cursor + 1)
                } else {
                    None
                }
            }
            TokenType::CharClass => {
                if cursor < self.tokens.len() && self.tokens[cursor].matches_char_class(&tok.text) {
                    Some(cursor + 1)
                } else {
                    None
                }
            }
            TokenType::NonTerminal => self.derive_rule(&tok.text, cursor),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn real_grammar() -> Grammar {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("workspace parent")
            .join("dsql_grammar.json");
        Grammar::load(&path)
    }

    #[test]
    fn tokenize_basic() {
        let toks = tokenize("CREATE TABLE t (id INT);");
        assert!(matches!(&toks[0], Tok::Word(s) if s == "CREATE"));
        assert!(matches!(&toks[1], Tok::Word(s) if s == "TABLE"));
        assert!(matches!(&toks[3], Tok::Punct('(')));
    }

    #[test]
    fn tokenize_strips_trailing_semicolon() {
        let toks = tokenize("BEGIN;");
        assert_eq!(toks.len(), 1);
    }

    #[test]
    fn loads_real_grammar() {
        let g = real_grammar();
        assert!(g.rules.len() > 100);
        eprintln!("Loaded {} rules", g.rules.len());
    }

    #[test]
    fn accepts_basic_create_table() {
        let g = real_grammar();
        // Smoke: the grammar should accept canonical DSQL CREATE TABLE.
        assert!(g.accepts("CREATE TABLE t (id INT)"));
    }

    #[test]
    fn rejects_create_temp_table() {
        let g = real_grammar();
        // DSQL's grammar has OptTemp empty — TEMP TABLE not allowed.
        assert!(!g.accepts("CREATE TEMP TABLE t (id INT)"));
    }

    /// Sample real DSQL statements to sanity-check oracle behaviour.
    /// Run with `--ignored --nocapture`.
    #[test]
    #[ignore = "diagnostic"]
    fn diag_expr_list() {
        let g = real_grammar();
        let toks = tokenize("NUMERIC(10,2)");
        eprintln!("tokens: {:?}", toks);
        let mut ctx = DerivCtx::new(&g, &toks);
        eprintln!("Numeric: {:?}", ctx.derive_rule("Numeric", 0));
        eprintln!(
            "opt_type_modifiers at 1: {:?}",
            ctx.derive_rule("opt_type_modifiers", 1)
        );
        eprintln!("expr_list at 2: {:?}", ctx.derive_rule("expr_list", 2));
        eprintln!("a_expr at 2: {:?}", ctx.derive_rule("a_expr", 2));
        eprintln!("Typename: {:?}", ctx.derive_rule("Typename", 0));
        eprintln!("SimpleTypename: {:?}", ctx.derive_rule("SimpleTypename", 0));
    }

    #[test]
    #[ignore = "diagnostic"]
    fn smoke_sample_statements() {
        let g = real_grammar();
        let cases: &[(&str, &str)] = &[
            // expected accept
            ("a", "CREATE TABLE t (id BIGINT PRIMARY KEY)"),
            ("a", "CREATE INDEX ASYNC idx ON t(c)"),
            ("a", "DROP TABLE t"),
            ("a", "ALTER TABLE t ADD COLUMN x INT"),
            ("a", "COMMIT"),
            ("a", "CREATE TABLE t (col NUMERIC(10,2))"),
            ("a", "CREATE TABLE t (col VARCHAR(100))"),
            // expected reject (DSQL restrictions)
            ("r", "CREATE TEMP TABLE t (id INT)"),
            ("r", "CREATE INDEX idx ON t(c)"), // missing ASYNC
            ("r", "VACUUM FULL t"),
            ("r", "LISTEN ch"),
            ("r", "REINDEX TABLE t"),
            // edge cases
            ("?", "SELECT 1"),
            ("?", "INSERT INTO t VALUES (1)"),
            ("?", "TRUNCATE TABLE t"),
        ];
        for (expected, sql) in cases {
            let t0 = std::time::Instant::now();
            let actual = g.accepts(sql);
            let elapsed = t0.elapsed().as_micros();
            eprintln!("[expected={expected}] accept={actual} ({elapsed}us): {sql}");
        }
    }
}
