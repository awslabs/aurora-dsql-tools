# Grammar Recognizer Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Add a test-time recognizer that parses `dsql_grammar.ebnf` into a runtime `chumsky` parser, then wire it into a CI test that asserts dsql-lint and the grammar agree on every existing test case — surfacing drift as a burndown TODO list rather than hard-blocking on day one.

**Architecture:** EBNF text → typed `Grammar` AST (small hand-written EBNF parser) → `chumsky` recognizer built programmatically from the AST. Two carve-outs handle the EBNF's quirks: pratt-combinator for left-recursive expression productions (`a_expr`, `b_expr`), and a fixed lexer for undefined terminals (`identifier`, `string_literal`, etc.). The recognizer is test-only — never linked into the shipped binary. A single test loops over dsql-lint's existing `ERROR_CASES` and supported-type matrix, asserting recognizer/dsql-lint agreement. Disagreements get listed in a checked-in `KNOWN_DRIFT.md` file; CI fails on *new* drift but tolerates known items. Goal: drive the list to zero.

**Tech Stack:** Rust 2021, `chumsky` 0.10 (dev-dep), no runtime additions.

**Source pivot:** Replaces the closed PR #55 (corpus-based approach) with the recognizer-based approach the user actually wanted. Background discussion: see superseded designs deleted from `docs/plans/` on `feat/grammar-corpus` (closed branch).

**Spike findings (2026-05-15):** Grammar is verified DSQL-specific. `OptTemp = ;`, `OptInherit = ;`, `OptPartitionSpec = ;` are all empty (rejecting their respective syntax). No `SERIAL` token in the grammar. `ConstraintElem` only contains `CHECK`, no `FOREIGN KEY`. Proceeding with the recognizer approach.

---

## Working assumptions

- **Grammar evolves.** `dsql_grammar.ebnf` is a vendored upstream artifact and will be replaced wholesale on updates. The recognizer must consume it as-is — no preprocessing step that requires re-running on grammar updates.
- **Fail-on-new-drift, tolerate-known-drift.** A `KNOWN_DRIFT.md` file lists currently-tolerated divergences. CI fails if any new drift appears, but allows known items to persist. The list shrinks over time as rules are added or the recognizer improves.
- **Cluster tests stay authoritative.** Per the user's tenet, all rules/fixes/errors are validated against a real DSQL cluster. The grammar oracle is a *second* signal that surfaces drift to maintainers; it is not the truth source for correctness.
- **`sqlparser-dsql` stays on the runtime path.** Unchanged. The recognizer never runs in the shipped binary.
- **Single-statement scope.** The recognizer parses a single statement at a time. Multi-statement input is dsql-lint's `split_statements` job (already done at runtime); the test passes one statement at a time to the recognizer.

---

## What "done enough to land" means

This is iterative work. The first landing bar is:

1. **Loader** parses `dsql_grammar.ebnf` into a typed `Grammar` AST without panicking on any production.
2. **Recognizer** can be constructed for the start production and any production reachable from it.
3. **Carve-outs** handle `a_expr` / `b_expr` (left-recursion via pratt) and the documented undefined terminals.
4. **Drift test** runs over the full set of cases in `dsql-lint/tests/integration_test.rs` (`ERROR_CASES`, `ADDITIONAL_ERROR_CASES`, `FALSE_POSITIVE_CASES`, `common::SUPPORTED_TYPES`, `common::CLEAN_STATEMENTS`).
5. Every disagreement is either fixed or copied verbatim into the `EXPECTED_DRIFT` const in `tests/grammar_oracle/drift.rs`.
6. CI fails on any disagreement not in `EXPECTED_DRIFT`. CI also fails on stale entries (entries that no longer disagree), so the list naturally shrinks.

Coverage of every production in the EBNF is **not** a landing requirement. The recognizer only needs to cover what's exercised by the existing test cases. Productions never reached by the test corpus stay un-validated by the oracle until someone writes a test that hits them — that's intentional, not a gap.

---

## Repo orientation (read before starting)

- Crate root: [`dsql-lint/`](../../dsql-lint/) (note: workspace member; cargo commands need to run from inside this directory or use `--manifest-path`)
- Library entry: [`dsql-lint/src/lib.rs`](../../dsql-lint/src/lib.rs) (re-exports `lint_sql`, `fix_sql`, `Diagnostic`, `FixResult`, `LintRule`)
- Lint engine: [`dsql-lint/src/lint.rs`](../../dsql-lint/src/lint.rs)
- Rules: [`dsql-lint/src/rules/errors.rs`](../../dsql-lint/src/rules/errors.rs)
- Existing tests we'll oracle against: [`dsql-lint/tests/integration_test.rs`](../../dsql-lint/tests/integration_test.rs) and [`dsql-lint/tests/common/mod.rs`](../../dsql-lint/tests/common/mod.rs)
- Grammar file: ✦ NOT YET IN MAIN ✦ — must be cherry-picked from the closed `feat/grammar-corpus` branch (commit `ce33353` has the file as `dsql_grammar.ebnf` at repo root)

---

## Phase 0 — Foundation

### Task 0.1: Branch off main with just the EBNF

**Files:**
- Create: `dsql_grammar.ebnf` (cherry-picked from closed branch)

**Step 0.1.1: Confirm branch state**

Run: `git status && git log --oneline -3`
Expected: on `feat/grammar-recognizer` branched from current `main` (after the EBNF was removed).

**Step 0.1.2: Bring back the EBNF file only**

Run: `git checkout feat/grammar-corpus -- dsql_grammar.ebnf && git status`
Expected: shows `dsql_grammar.ebnf` as new file in working tree.

**Step 0.1.3: Commit the EBNF**

```bash
git add dsql_grammar.ebnf
git commit -m "chore(grammar): vendor dsql_grammar.ebnf from upstream"
```

This is the only artifact carried over from the closed branch.

---

### Task 0.2: Add chumsky as dev-dependency

**Files:**
- Modify: `dsql-lint/Cargo.toml`

**Step 0.2.1: Add the dev-dep**

Edit `dsql-lint/Cargo.toml`. Add (or extend) the `[dev-dependencies]` block:

```toml
[dev-dependencies]
chumsky = "0.10"
```

(Pin to 0.10 specifically; 0.10 has the API we want for programmatic parser construction. Newer versions may exist but 0.10 is what this plan targets.)

**Step 0.2.2: Verify it resolves**

Run from `dsql-lint/`: `cargo build --tests 2>&1 | tail -10`
Expected: builds clean, downloads chumsky and its deps. No new warnings.

**Step 0.2.3: Commit**

```bash
git add dsql-lint/Cargo.toml dsql-lint/Cargo.lock
git commit -m "chore(deps): add chumsky as dev-dep for grammar oracle"
```

---

## Phase 1 — EBNF loader

This phase parses the EBNF text into a typed `Grammar` AST. Pure I/O-free parsing. Easy to TDD; everything else builds on it.

### Task 1.1: Scaffold the test module

**Files:**
- Create: `dsql-lint/tests/grammar_oracle.rs`
- Create: `dsql-lint/tests/grammar_oracle/mod.rs`
- Create: `dsql-lint/tests/grammar_oracle/ebnf.rs`

cargo treats every `tests/*.rs` file as a separate integration test crate. Helper modules go in `tests/<name>/mod.rs` and submodules. Mirror [`tests/common/mod.rs`](../../dsql-lint/tests/common/mod.rs).

**Step 1.1.1: Create the entry point**

`dsql-lint/tests/grammar_oracle.rs`:

```rust
//! Grammar oracle: test-time recognizer for `dsql_grammar.ebnf`.
//!
//! Loads the upstream EBNF, builds a chumsky recognizer, and asserts that
//! dsql-lint and the recognizer agree on the test corpus. Drift is tracked
//! in `KNOWN_DRIFT.md` with the goal of driving the list to zero.

mod grammar_oracle;
```

**Step 1.1.2: Create the helper module skeleton**

`dsql-lint/tests/grammar_oracle/mod.rs`:

```rust
//! Helpers for the grammar oracle integration test.
//!
//! Submodules:
//! - `ebnf`: parse `dsql_grammar.ebnf` text into a typed `Grammar` AST.

pub mod ebnf;
```

**Step 1.1.3: Create the ebnf module skeleton**

`dsql-lint/tests/grammar_oracle/ebnf.rs`:

```rust
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
```

**Step 1.1.4: Add the first failing test**

Append to `dsql-lint/tests/grammar_oracle/ebnf.rs`:

```rust
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
```

**Step 1.1.5: Run, see it fail**

Run from `dsql-lint/`:
`cargo test -p dsql-lint --test grammar_oracle parse_single_terminal_production`
Expected: panic at `not yet implemented: Task 1.2+`.

**Step 1.1.6: Commit**

```bash
git add dsql-lint/tests/grammar_oracle.rs dsql-lint/tests/grammar_oracle/
git commit -m "test(grammar-oracle): scaffold ebnf parser module"
```

---

### Task 1.2: Minimal EBNF parser — terminals only

**Files:**
- Modify: `dsql-lint/tests/grammar_oracle/ebnf.rs`

**Step 1.2.1: Implement just enough to pass the first test**

Replace the `todo!()` body with a minimal hand-rolled parser. EBNF productions look like:

```
Identifier = expression ;
```

For Task 1.2 we only need: identifier on the left, single quoted terminal on the right, semicolon-terminated, one production per `parse_grammar` call.

```rust
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
```

**Step 1.2.2: Run, see it pass**

Run from `dsql-lint/`:
`cargo test -p dsql-lint --test grammar_oracle parse_single_terminal_production`
Expected: 1 passed.

**Step 1.2.3: Commit**

```bash
git add dsql-lint/tests/grammar_oracle/ebnf.rs
git commit -m "feat(grammar-oracle): minimal EBNF parser for single terminal"
```

---

### Task 1.3: EBNF parser — non-terminals, sequence, choice

**Files:**
- Modify: `dsql-lint/tests/grammar_oracle/ebnf.rs`

**Step 1.3.1: Add failing tests for the new features**

Append to the `mod tests` block:

```rust
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
```

**Step 1.3.2: Run — they fail**

Run: `cargo test -p dsql-lint --test grammar_oracle parse_`
Expected: 4 fail (the new ones), 1 pass (Task 1.2's).

**Step 1.3.3: Replace `read_terminal` callsite with a recursive `read_alternation`**

The right shape: each production body is an *alternation* (top-level `|`). Each alternative is a *sequence* (one or more atoms). Each atom is a terminal, non-terminal, or grouped expression.

Add to `ebnf.rs`:

```rust
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

fn read_sequence<I: Iterator<Item = char>>(
    chars: &mut std::iter::Peekable<I>,
    line: &mut usize,
) -> Result<Production, ParseError> {
    let mut atoms = vec![read_atom(chars, line)?];
    loop {
        skip_ws_and_comments(chars, line);
        match chars.peek() {
            Some(&c) if c == ';' || c == '|' || c == ']' || c == '}' || c == ')' => break,
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
```

Replace the body-reading line in `parse_grammar` from `let body = read_terminal(...)` to:

```rust
let body = read_alternation(&mut chars, &mut line)?;
productions.insert(name, body);
```

(Drop the now-unused single-terminal codepath from `parse_grammar`.)

**Step 1.3.4: Run — all pass**

Run: `cargo test -p dsql-lint --test grammar_oracle parse_`
Expected: 5 passed.

**Step 1.3.5: Commit**

```bash
git add dsql-lint/tests/grammar_oracle/ebnf.rs
git commit -m "feat(grammar-oracle): EBNF parser supports sequence, choice, non-terminals"
```

---

### Task 1.4: EBNF parser — optional `[ ... ]`, repetition `{ ... }`, grouping `( ... )`

**Files:**
- Modify: `dsql-lint/tests/grammar_oracle/ebnf.rs`

**Step 1.4.1: Add failing tests**

Append to `mod tests`:

```rust
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
```

**Step 1.4.2: Run — fail**

Run: `cargo test -p dsql-lint --test grammar_oracle parse_optional parse_repetition parse_grouping`
Expected: 3 fail.

**Step 1.4.3: Extend `read_atom`**

Replace the `read_atom` function with:

```rust
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
```

**Step 1.4.4: Run — all pass**

Expected: 8 tests pass total.

**Step 1.4.5: Commit**

```bash
git add dsql-lint/tests/grammar_oracle/ebnf.rs
git commit -m "feat(grammar-oracle): EBNF parser supports optional, repetition, grouping"
```

---

### Task 1.5: Smoke-test against the real EBNF

**Files:**
- Modify: `dsql-lint/tests/grammar_oracle/ebnf.rs`

**Step 1.5.1: Add an integration test that loads `dsql_grammar.ebnf`**

Append to `mod tests`:

```rust
#[test]
fn parses_full_dsql_grammar_ebnf() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("workspace parent")
        .join("dsql_grammar.ebnf");
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    let g = parse_grammar(&text).expect("grammar should parse");
    // Expect a healthy number of productions; exact count is allowed to drift
    // as upstream evolves, but a regression to <100 means we lost productions.
    assert!(
        g.productions.len() > 100,
        "expected >100 productions, got {}",
        g.productions.len()
    );
    // Spot-check a known production exists.
    assert!(g.productions.contains_key("CreateStmt"));
}
```

**Step 1.5.2: Run**

Run: `cargo test -p dsql-lint --test grammar_oracle parses_full`
Expected: pass. If fails, the EBNF has constructs the parser doesn't handle. Add tests that minimally reproduce, then extend the parser. **Do NOT skip productions silently.** The whole point is to consume the grammar as-is.

Common things this might surface that need handling:
- Comments (e.g. `(* this is a comment *)` or `// like this`) — extend `skip_ws_and_comments`
- Hyphens in identifiers? — unlikely for EBNF but worth checking
- Multi-line productions (productions that span lines with `|` continuation)
- Empty productions (e.g. `OptTableSpace = ;` or `OptTableSpace = ;`)
- Trailing whitespace / final-line issues

The grammar at commit `ce33353` is 623 lines. From inspection it uses bare names, single-quoted terminals, `|` for choice, `[ ]` for optional, `{ }` for repetition. No comments visible in the sample. Empty production bodies (`X = ;`) appear, e.g. `OptTableSpace = ;` — this is a quirk you'll need to handle: an empty alternation should parse as `Production::Sequence(vec![])` (empty sequence = matches nothing = epsilon). You may need to check for this in `read_sequence` / `read_alternation` and produce `Sequence(vec![])` when there are no atoms before the terminator.

**Step 1.5.3: Iterate until the test passes**

Each iteration that adds parser support should land as its own small commit. Don't bulk-fix; let the test surface things one at a time.

**Step 1.5.4: Final commit**

```bash
git add dsql-lint/tests/grammar_oracle/ebnf.rs
git commit -m "test(grammar-oracle): parse full dsql_grammar.ebnf"
```

(Possibly multiple commits if you needed to extend the parser.)

---

## Phase 2 — Recognizer

This phase builds a `chumsky` parser from the `Grammar` AST. The recognizer's only job: `accepts(sql) -> bool`. No error reporting, no AST extraction. Just yes/no.

### Task 2.1: Recognizer skeleton

**Files:**
- Create: `dsql-lint/tests/grammar_oracle/recognizer.rs`
- Modify: `dsql-lint/tests/grammar_oracle/mod.rs`

**Step 2.1.1: Add the module**

`dsql-lint/tests/grammar_oracle/recognizer.rs`:

```rust
//! Build a chumsky recognizer from a `Grammar` AST.
//!
//! The recognizer is intentionally minimal: it only answers
//! `accepts(sql) -> bool`. We don't extract any structured output —
//! drift detection only needs a yes/no verdict.

use crate::grammar_oracle::ebnf::{Grammar, Production};

pub struct Recognizer {
    // Filled in by Task 2.2+
    _grammar: Grammar,
    _start: String,
}

impl Recognizer {
    pub fn build(grammar: Grammar, start: &str) -> Self {
        Self {
            _grammar: grammar,
            _start: start.to_string(),
        }
    }

    /// Returns true if `sql` matches the start production.
    pub fn accepts(&self, _sql: &str) -> bool {
        todo!("Task 2.2+")
    }
}
```

Update `dsql-lint/tests/grammar_oracle/mod.rs`:

```rust
//! Helpers for the grammar oracle integration test.

pub mod ebnf;
pub mod recognizer;
```

**Step 2.1.2: Add a failing smoke test**

Append to `recognizer.rs`:

```rust
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
```

**Step 2.1.3: Run, see it fail with `todo!`**

Run: `cargo test -p dsql-lint --test grammar_oracle recognizer_accepts_single_terminal`
Expected: panic.

**Step 2.1.4: Commit**

```bash
git add dsql-lint/tests/grammar_oracle/
git commit -m "test(grammar-oracle): scaffold chumsky recognizer module"
```

---

### Task 2.2: Recognizer for terminals + non-terminals

**Files:**
- Modify: `dsql-lint/tests/grammar_oracle/recognizer.rs`

**Step 2.2.1: Implement enough to handle terminal start productions**

The grammar's terminals are SQL keywords like `'CREATE'`, `'TABLE'`. Matching is case-insensitive (SQL convention). Whitespace between tokens is significant: tokens are whitespace-separated, but multiple whitespace is one.

Replace the body with a minimal chumsky-based recognizer:

```rust
use chumsky::prelude::*;

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

    pub fn accepts(&self, sql: &str) -> bool {
        let parser = self.build_parser_for(&self.start);
        parser.padded().then_ignore(end()).parse(sql).is_ok()
    }

    fn build_parser_for(&self, name: &str) -> Boxed<'_, '_, &str, (), extra::Default> {
        let prod = self
            .grammar
            .productions
            .get(name)
            .unwrap_or_else(|| panic!("unknown production: {name}"));
        self.build_parser(prod).boxed()
    }

    fn build_parser<'a>(
        &'a self,
        prod: &'a Production,
    ) -> impl Parser<'a, &'a str, (), extra::Default> + Clone + 'a {
        match prod {
            Production::Terminal(t) => {
                // Case-insensitive keyword match; require word boundary on both sides
                // when the terminal starts with a letter.
                let t_owned = t.clone();
                let p = any()
                    .repeated()
                    .exactly(t_owned.len())
                    .collect::<String>()
                    .filter(move |s: &String| s.eq_ignore_ascii_case(&t_owned))
                    .ignored();
                p.padded().boxed()
            }
            Production::NonTerminal(n) => self.build_parser_for(n),
            // Other variants land in Tasks 2.3+
            _ => todo!("variant not yet handled in Task 2.2: {:?}", prod),
        }
    }
}
```

**Note:** This is not the final shape — chumsky's lifetime machinery makes building parsers from a runtime AST harder than building them from compile-time code. The recursive `build_parser` will likely hit recursion-with-self-referencing-lifetime issues.

The realistic shape for Task 2.2 onwards is to **build the parser eagerly into a `HashMap<String, Boxed<...>>`** at `Recognizer::build` time, using chumsky's `recursive()` combinator for the cycles. That's a larger refactor than fits in one TDD step. Two options:

- **Option A (incremental):** Get the simple non-recursive case working first (terminals, non-terminals that don't cycle). Land that. Then extend to recursion in Task 2.5.
- **Option B (upfront):** Spend Task 2.2 fully on the recognizer architecture, including `recursive()` setup, and treat the rest of Phase 2 as filling in the `Production` variants.

**Recommend Option B.** Half-built recognizer infrastructure is harder to extend than to write from scratch. Allow Task 2.2 to be a longer task than the rest of Phase 1's tasks — budget 1-2 hours of work and 5-10 commits inside it.

The implementer should:
1. Read chumsky 0.10's docs for `recursive()`, `Boxed`, and the lifetime patterns for runtime-constructed parsers. The chumsky tutorial example for "build parser at runtime from a grammar AST" is the right model. https://docs.rs/chumsky/latest/chumsky/recursive/index.html
2. Decide on a parser representation (suggested: `HashMap<String, Recursive<'_, &'_ str, (), extra::Default>>` populated in a single pass over the grammar).
3. Get the simple test passing.
4. **STOP and report back** before tackling sequences/choice. The shape of the recognizer affects every subsequent task.

**Step 2.2.2: Run, debug, iterate**

Until `recognizer_accepts_single_terminal` passes.

**Step 2.2.3: Commit**

```bash
git add dsql-lint/tests/grammar_oracle/recognizer.rs
git commit -m "feat(grammar-oracle): recognizer for terminal start productions"
```

---

### Task 2.3: Recognizer for sequence + choice

**Files:**
- Modify: `dsql-lint/tests/grammar_oracle/recognizer.rs`

**Step 2.3.1: Add tests**

```rust
#[test]
fn recognizer_accepts_sequence() {
    let g = parse_grammar("Hello = 'hello' 'world' ;").unwrap();
    let r = Recognizer::build(g, "Hello");
    assert!(r.accepts("hello world"));
    assert!(r.accepts("HELLO WORLD"));    // case-insensitive keyword
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
```

**Step 2.3.2: Implement the new variants**

Add `Production::Sequence` and `Production::Choice` cases to `build_parser`. Use chumsky's `then_ignore` chain for sequence and `or` for choice.

**Step 2.3.3: Run, commit**

```bash
git add dsql-lint/tests/grammar_oracle/recognizer.rs
git commit -m "feat(grammar-oracle): recognizer for sequence and choice"
```

---

### Task 2.4: Recognizer for optional + repetition

**Files:**
- Modify: `dsql-lint/tests/grammar_oracle/recognizer.rs`

**Step 2.4.1: Add tests**

```rust
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
```

**Step 2.4.2: Implement**

Use chumsky's `or_not()` for optional and `repeated().collect::<()>()` (or equivalent) for repetition.

**Step 2.4.3: Run, commit**

```bash
git add dsql-lint/tests/grammar_oracle/recognizer.rs
git commit -m "feat(grammar-oracle): recognizer for optional and repetition"
```

---

### Task 2.5: Recognizer for cycles (mutual + self recursion)

**Files:**
- Modify: `dsql-lint/tests/grammar_oracle/recognizer.rs`

This is the hard one. Most realistic SQL grammars have at least mutual recursion (statement → expression → ... → statement). chumsky's `recursive()` combinator handles it but requires careful setup when building from a runtime AST.

**Step 2.5.1: Add tests**

```rust
#[test]
fn recognizer_handles_mutual_recursion() {
    let g = parse_grammar("\
        A = 'a' [ B ] ;
        B = 'b' [ A ] ;
    ").unwrap();
    let r = Recognizer::build(g, "A");
    assert!(r.accepts("a"));
    assert!(r.accepts("a b"));
    assert!(r.accepts("a b a"));
    assert!(r.accepts("a b a b"));
}

#[test]
fn recognizer_handles_right_recursion() {
    let g = parse_grammar("\
        List = 'item' [ ',' List ] ;
    ").unwrap();
    let r = Recognizer::build(g, "List");
    assert!(r.accepts("item"));
    assert!(r.accepts("item , item"));
    assert!(r.accepts("item , item , item"));
}
```

Note: do NOT add a left-recursion test here — that's Task 3.1's domain. Right-recursion works fine in chumsky; left-recursion needs pratt.

**Step 2.5.2: Refactor `Recognizer::build` to populate parsers via `recursive()`**

The pattern:

```rust
// pseudo-Rust
let parsers: HashMap<String, Recursive<...>> = HashMap::new();
for name in grammar.productions.keys() {
    parsers.insert(name.clone(), recursive(|p| { /* placeholder */ }));
}
// Now define each parser, referencing siblings via the map.
for (name, prod) in &grammar.productions {
    let p = build_parser(prod, &parsers);
    parsers.get_mut(name).unwrap().define(p);
}
```

The exact mechanics depend on chumsky 0.10's recursive() API, which the implementer must consult. Get one mutual-recursion test passing first; the rest follow.

**Step 2.5.3: Run, debug, commit**

```bash
git add dsql-lint/tests/grammar_oracle/recognizer.rs
git commit -m "feat(grammar-oracle): recognizer handles cycles via chumsky::recursive"
```

---

## Phase 3 — Carve-outs

### Task 3.1: Pratt for `a_expr` / `b_expr` (left-recursion)

The EBNF's `a_expr` production at line 30 of `dsql_grammar.ebnf` (in the closed branch) is a classic left-recursive arithmetic expression grammar. Naive chumsky recursion will infinite-loop on it.

**Files:**
- Modify: `dsql-lint/tests/grammar_oracle/recognizer.rs`

**Step 3.1.1: Detect left-recursion by shape**

Before building a parser for production X, check if any alternative starts with a non-terminal that resolves (transitively) to X. If yes, this production is left-recursive and needs pratt treatment.

**Step 3.1.2: Hardcode operator precedence**

PostgreSQL has ~30 operators with documented precedence: https://www.postgresql.org/docs/current/sql-syntax-lexical.html#SQL-PRECEDENCE-TABLE

Encode them in a const table:

```rust
const OPERATOR_PRECEDENCE: &[(&str, u8, Associativity)] = &[
    ("OR", 1, Left),
    ("AND", 2, Left),
    ("NOT", 3, Right),
    ("=", 4, Nonassoc),
    // ...
    ("*", 8, Left),
    ("/", 8, Left),
];
```

**Step 3.1.3: Use chumsky's `pratt` combinator**

chumsky 0.10 has a `pratt` module for exactly this. Build the expression parser via `pratt` with the precedence table above. The non-recursive parts (atoms — column refs, literals, function calls) are the operand parser; the operators come from the table.

**Step 3.1.4: Tests**

Add tests for the left-recursive case:

```rust
#[test]
fn recognizer_handles_a_expr_arithmetic() {
    let path = workspace_root().join("dsql_grammar.ebnf");
    let g = parse_grammar(&std::fs::read_to_string(path).unwrap()).unwrap();
    let r = Recognizer::build(g, "a_expr");
    assert!(r.accepts("1 + 2"));
    assert!(r.accepts("1 + 2 * 3"));
    assert!(r.accepts("(1 + 2) * 3"));
    assert!(!r.accepts("1 + + 2"));
}
```

**Step 3.1.5: Commit**

```bash
git add dsql-lint/tests/grammar_oracle/recognizer.rs
git commit -m "feat(grammar-oracle): pratt-based handling for left-recursive a_expr"
```

---

### Task 3.2: Undefined terminals (`identifier`, `string_literal`, etc.)

The EBNF references `identifier`, `string_literal`, `integer_constant`, `float_constant`, `operator`, `binary_string`, `hex_string` without defining them.

**Files:**
- Modify: `dsql-lint/tests/grammar_oracle/recognizer.rs`

**Step 3.2.1: Map each undefined terminal to a small chumsky parser**

```rust
fn parser_for_undefined(name: &str) -> Option<impl Parser<'_, &'_ str, (), extra::Default> + Clone> {
    Some(match name {
        "identifier" => /* `[A-Za-z_][A-Za-z0-9_]*` (also handle quoted "..." identifiers) */,
        "string_literal" => /* `'...'` with escaped quotes */,
        "integer_constant" => /* `[0-9]+` */,
        "float_constant" => /* `[0-9]+\.[0-9]+` (or scientific) */,
        "operator" => /* PostgreSQL operator chars */,
        "binary_string" => /* `B'01010'` */,
        "hex_string" => /* `X'DEADBEEF'` */,
        _ => return None,
    })
}
```

**Step 3.2.2: Wire into `build_parser`**

In the `Production::NonTerminal` branch, check the undefined-terminal map first; if a name maps, use that parser; otherwise look up in `grammar.productions`.

**Step 3.2.3: Tests**

```rust
#[test]
fn recognizer_handles_identifier() {
    let g = parse_grammar("X = identifier ;").unwrap();
    let r = Recognizer::build(g, "X");
    assert!(r.accepts("foo"));
    assert!(r.accepts("orders_2024"));
    assert!(!r.accepts("123abc"));
}

#[test]
fn recognizer_handles_string_literal() {
    let g = parse_grammar("X = string_literal ;").unwrap();
    let r = Recognizer::build(g, "X");
    assert!(r.accepts("'hello'"));
    assert!(r.accepts("''"));
    assert!(!r.accepts("hello"));
}
```

**Step 3.2.4: Commit**

```bash
git add dsql-lint/tests/grammar_oracle/recognizer.rs
git commit -m "feat(grammar-oracle): lexer rules for EBNF undefined terminals"
```

---

### Task 3.3: First end-to-end test against real DSQL SQL

**Files:**
- Modify: `dsql-lint/tests/grammar_oracle/recognizer.rs`

**Step 3.3.1: Pick a small handful of statements**

```rust
#[test]
fn recognizer_accepts_known_valid_dsql_sql() {
    let path = workspace_root().join("dsql_grammar.ebnf");
    let g = parse_grammar(&std::fs::read_to_string(path).unwrap()).unwrap();
    let r = Recognizer::build(g, "stmt");  // or whatever the start production is

    assert!(r.accepts("CREATE TABLE t (id BIGINT PRIMARY KEY)"));
    assert!(r.accepts("CREATE INDEX ASYNC idx ON t(c)"));
    assert!(!r.accepts("CREATE TABLE t (id SERIAL PRIMARY KEY)"));     // SERIAL not in grammar
    assert!(!r.accepts("CREATE INDEX idx ON t(c)"));                    // missing ASYNC
}
```

The actual start production name needs to be confirmed by reading the EBNF. If the grammar has multiple top-level alternatives wrapped in something like `stmt = SelectStmt | CreateStmt | ... ;`, that's the start.

**Step 3.3.2: Run and triage**

If the recognizer rejects valid SQL: bug in the recognizer or the grammar references something we missed. Investigate.

If the recognizer accepts invalid SQL: the grammar is more permissive than we thought (legitimately) OR the recognizer has a bug.

**Step 3.3.3: Commit**

```bash
git add dsql-lint/tests/grammar_oracle/recognizer.rs
git commit -m "test(grammar-oracle): smoke-test against representative DSQL SQL"
```

---

## Phase 4 — Drift detection

### Task 4.1: Drift test wired against `ERROR_CASES`

**Files:**
- Create: `dsql-lint/tests/grammar_oracle/drift.rs`
- Modify: `dsql-lint/tests/grammar_oracle/mod.rs` (add `pub mod drift;`)
- Modify: `dsql-lint/tests/grammar_oracle.rs` (call the test)

**Step 4.1.1: Reuse existing test corpus directly**

The cases we want to oracle against already live in [`dsql-lint/tests/integration_test.rs`](../../dsql-lint/tests/integration_test.rs):
- `ERROR_CASES` — SQL that should be rejected
- `ADDITIONAL_ERROR_CASES` — same
- `FALSE_POSITIVE_CASES` — SQL that should NOT trip a specific rule (proxy for "should be accepted")
- `common::CLEAN_STATEMENTS` and `common::SUPPORTED_TYPES` — SQL that must be accepted

**Don't duplicate.** Move the constants we need into [`tests/common/mod.rs`](../../dsql-lint/tests/common/mod.rs) (or a new common submodule) and `pub use` from both `integration_test.rs` and the new oracle test. Check what's already in `common/` first; some constants may already be public.

**Step 4.1.2: Implement the drift check**

Create `dsql-lint/tests/grammar_oracle/drift.rs`:

```rust
//! Drift detection: assert dsql-lint and the recognizer agree on every
//! known case. New disagreements fail CI; expected disagreements are
//! listed in `EXPECTED_DRIFT` (see below).

use crate::grammar_oracle::ebnf::parse_grammar;
use crate::grammar_oracle::recognizer::Recognizer;
use dsql_lint::lint_sql;
use std::path::Path;
use std::sync::OnceLock;

#[derive(Debug, Clone)]
pub struct Disagreement {
    pub sql: String,
    pub kind: DisagreementKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DisagreementKind {
    /// dsql-lint says error; grammar accepts. Suggests grammar relaxed,
    /// or dsql-lint is over-flagging.
    LintFlagsGrammarAccepts,
    /// Grammar rejects; dsql-lint says clean. Suggests dsql-lint is missing a rule.
    GrammarRejectsLintQuiet,
}

fn recognizer() -> &'static Recognizer {
    static R: OnceLock<Recognizer> = OnceLock::new();
    R.get_or_init(|| {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("workspace parent")
            .join("dsql_grammar.ebnf");
        let text = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
        let g = parse_grammar(&text).expect("grammar parse");
        Recognizer::build(g, "stmt") // confirmed during recognizer wiring
    })
}

pub fn collect() -> Vec<Disagreement> {
    let r = recognizer();
    let mut out = Vec::new();

    // Cases dsql-lint rejects. Grammar should also reject; if it accepts,
    // that's a disagreement.
    for sql in cases::REJECT_CASES {
        let lint_flags = !lint_sql(sql).is_empty();
        let grammar_accepts = r.accepts(strip_trailing_semi(sql));
        match (lint_flags, grammar_accepts) {
            (true, true) => out.push(Disagreement {
                sql: sql.to_string(),
                kind: DisagreementKind::LintFlagsGrammarAccepts,
            }),
            (false, false) => out.push(Disagreement {
                sql: sql.to_string(),
                kind: DisagreementKind::GrammarRejectsLintQuiet,
            }),
            _ => {}
        }
    }

    // Cases dsql-lint accepts. Grammar should also accept; if it rejects,
    // that's a disagreement.
    for sql in cases::ACCEPT_CASES {
        let lint_flags = !lint_sql(sql).is_empty();
        let grammar_accepts = r.accepts(strip_trailing_semi(sql));
        match (lint_flags, grammar_accepts) {
            (true, true) => out.push(Disagreement {
                sql: sql.to_string(),
                kind: DisagreementKind::LintFlagsGrammarAccepts,
            }),
            (false, false) => out.push(Disagreement {
                sql: sql.to_string(),
                kind: DisagreementKind::GrammarRejectsLintQuiet,
            }),
            _ => {}
        }
    }

    out
}

fn strip_trailing_semi(s: &str) -> &str {
    s.trim().trim_end_matches(';').trim_end()
}

mod cases {
    // Pulled from `tests/integration_test.rs` and `tests/common/mod.rs`.
    // Re-export from common/ rather than duplicating; this comment is here
    // as a navigation hint when grepping.
    pub use crate::common::{ACCEPT_CASES, REJECT_CASES};
}
```

(Adjust the `cases` module to match wherever you placed the shared constants.)

**Step 4.1.3: Add the `EXPECTED_DRIFT` opt-out**

In `dsql-lint/tests/grammar_oracle/drift.rs`, add a constant alongside the test:

```rust
/// Cases where the recognizer and dsql-lint legitimately disagree, accepted
/// for now. Each entry is the exact SQL string from the test corpus.
///
/// Adding to this list documents a known gap. The intent is that this list
/// shrinks over time — every removal corresponds to either a new dsql-lint
/// rule, a recognizer fix, or a grammar update.
///
/// Do not add cases without a comment explaining *why*.
pub const EXPECTED_DRIFT: &[&str] = &[
    // (initially empty — populated on first CI run)
];
```

**Step 4.1.4: Wire into the test entry**

`dsql-lint/tests/grammar_oracle.rs`:

```rust
//! Grammar oracle: see module docs in `grammar_oracle/`.

mod common; // shared with integration_test.rs
mod grammar_oracle;

use grammar_oracle::drift;

#[test]
fn dsql_lint_agrees_with_grammar() {
    let disagreements = drift::collect();
    let expected: std::collections::HashSet<&str> =
        drift::EXPECTED_DRIFT.iter().copied().collect();

    let unexpected: Vec<_> = disagreements
        .iter()
        .filter(|d| !expected.contains(d.sql.as_str()))
        .collect();

    if !unexpected.is_empty() {
        let mut msg = String::from("dsql-lint and grammar disagree on cases not in EXPECTED_DRIFT:\n\n");
        for d in &unexpected {
            msg.push_str(&format!("  [{:?}] {}\n", d.kind, d.sql));
        }
        msg.push_str(
            "\nFix by one of:\n  \
             - Add a rule to dsql-lint (most common)\n  \
             - Fix the recognizer if it's wrong\n  \
             - Add the SQL string to EXPECTED_DRIFT in tests/grammar_oracle/drift.rs \
             with a comment explaining why\n",
        );
        panic!("{msg}");
    }

    // Also fail if EXPECTED_DRIFT contains stale entries (i.e., the disagreement
    // no longer exists). Forces the list to shrink as fixes land.
    let actual: std::collections::HashSet<&str> =
        disagreements.iter().map(|d| d.sql.as_str()).collect();
    let stale: Vec<&str> = drift::EXPECTED_DRIFT
        .iter()
        .copied()
        .filter(|s| !actual.contains(s))
        .collect();
    if !stale.is_empty() {
        let mut msg = String::from("EXPECTED_DRIFT contains entries that no longer disagree:\n");
        for s in &stale {
            msg.push_str(&format!("  {s}\n"));
        }
        msg.push_str("\nRemove these entries — they're stale.\n");
        panic!("{msg}");
    }
}
```

This is the entire mechanic: a `const &[&str]` of opt-outs, no markdown file, no `BLESS=1`, no parsing. Adding an entry is editing one line of Rust. Removing an entry is one line. CI naturally drives the file toward zero — stale entries fail too.

**Step 4.1.5: Run, capture initial drift, populate `EXPECTED_DRIFT`**

Run from `dsql-lint/`: `cargo test -p dsql-lint --test grammar_oracle dsql_lint_agrees_with_grammar`
Expected: fails with the list of disagreements. Copy each SQL string into `EXPECTED_DRIFT` with a one-line `//` comment per entry. Don't triage upfront — copy verbatim. Triage happens in subsequent PRs as rules get fixed.

**Step 4.1.6: Re-run, confirm pass**

Run again. Expected: passes.

**Step 4.1.7: Commit**

```bash
git add dsql-lint/tests/grammar_oracle.rs dsql-lint/tests/grammar_oracle/drift.rs dsql-lint/tests/grammar_oracle/mod.rs dsql-lint/tests/common/mod.rs
git commit -m "feat(grammar-oracle): drift check vs dsql-lint with EXPECTED_DRIFT opt-out"
```

---

(Task 4.2 folded into 4.1 — the drift test runs against the full shared corpus from the start.)

---

## Phase 5 — Documentation and contributor surface

### Task 5.1: CONTRIBUTING update

**Files:**
- Modify: `CONTRIBUTING.md`

**Step 5.1.1: Append "Grammar oracle" section**

```markdown
## Grammar oracle

`dsql_grammar.ebnf` is the source of truth for what DSQL accepts. A
test-time recognizer asserts that dsql-lint and the grammar agree on every
case in `dsql-lint/tests/integration_test.rs`.

When the test fails, the message tells you the disagreement kind:

- **`GrammarRejectsLintQuiet`** — grammar rejects, dsql-lint silent.
  dsql-lint is missing a rule (most common case). Add one in
  `src/rules/errors.rs`.
- **`LintFlagsGrammarAccepts`** — dsql-lint flags it, grammar accepts.
  Either the grammar relaxed (remove/loosen the rule) or dsql-lint is
  over-flagging.

If a fix is non-trivial, add the SQL string to `EXPECTED_DRIFT` in
`tests/grammar_oracle/drift.rs` with a one-line `//` comment. The list
fails CI on stale entries, so it shrinks naturally as fixes land.

The grammar oracle is a second signal; cluster tests remain authoritative
for correctness.
```

**Step 5.1.2: Commit**

```bash
git add CONTRIBUTING.md
git commit -m "docs(contributing): grammar oracle workflow"
```

---

## Phase 6 — Final verification

### Task 6.1: Full local check + push

**Step 6.1.1: Full test run**

From `dsql-lint/`:
- `cargo test -p dsql-lint` → all green
- `cargo clippy -- -D warnings` → clean
- `cargo fmt --check` → clean

**Step 6.1.2: Sanity-test the opt-out mechanic**

Pick any entry from `EXPECTED_DRIFT` and delete it. Re-run the test. Expected: fails with that case listed as new disagreement. Restore the entry.

Then add a fake entry like `"NOT REAL SQL"`. Re-run. Expected: fails with "EXPECTED_DRIFT contains entries that no longer disagree". Remove the fake entry.

This proves both directions of the gate work.

**Step 6.1.3: Push and open PR**

```bash
git push -u origin feat/grammar-recognizer
gh pr create --title "feat(dsql-lint): grammar recognizer + drift oracle" --body "$(cat <<'EOF'
## Summary

Adds a test-time recognizer that parses `dsql_grammar.ebnf` into a chumsky parser and asserts dsql-lint and the grammar agree on every existing test case.

Drift is tolerated via `KNOWN_DRIFT.md` (burndown list, target: zero).

## Tech stack

- chumsky 0.10 as `[dev-dependencies]`
- No runtime additions — recognizer is test-only
- Hand-written EBNF parser (consumes the grammar as-is, no preprocessing step)
- Carve-outs for left-recursive expression productions (pratt) and undefined terminals (fixed lexer)

## What this does

- Loader: `dsql_grammar.ebnf` → typed `Grammar` AST
- Recognizer: `Grammar` → `chumsky` parser; exposes `accepts(sql) -> bool`
- Drift test: walks `tests/integration_test.rs` corpus, asserts dsql-lint and the grammar agree
- New drifts fail CI; known drifts in `KNOWN_DRIFT.md` are tolerated

## What this doesn't do

- No replacement of `sqlparser-dsql` on the runtime path
- No new shipped binary dependency
- No fixture corpus (we use the existing test cases)

## Test plan

- [ ] `cargo test -p dsql-lint` passes
- [ ] `cargo clippy -- -D warnings` clean
- [ ] `cargo fmt --check` clean
- [ ] `KNOWN_DRIFT.md` count is the initial drift count and shrinks via follow-ups
EOF
)"
```

---

## Risk register

| Risk | Probability | Mitigation |
|---|---|---|
| Phase 2 (chumsky recognizer) takes longer than budgeted | High | Phase 2.2 has a "stop and report back" gate so the architecture can be revisited before sinking time into wrong-shape parsers |
| EBNF has constructs the parser doesn't handle | Medium | Phase 1.5 forces this issue; each new construct lands as its own commit |
| Recognizer produces false-positive rejections (rejects valid SQL) | Medium | Phase 3.3 smoke-tests against known-valid SQL; KNOWN_DRIFT.md catches the rest as `grammar_rejects_valid_sql` |
| Pratt + chumsky combination is finicky | Medium | Hardcoded precedence table is small (~30 entries); failure mode is "left-recursive expressions don't match" which is testable |
| Drift list grows instead of shrinking | Low (process risk) | CONTRIBUTING.md reinforces "fix > add to list"; PR review checks each addition has a real reason |

## Notes for the implementer

- **TDD throughout Phase 1.** Each EBNF feature lands as one failing test → one passing test → one commit.
- **Phase 2.2 is the big risk.** Budget hours, not minutes. Don't move on until terminal-only recognition works end-to-end with `recursive()` infrastructure in place.
- **Don't rush `EXPECTED_DRIFT`.** The list at the end of Phase 4 is the *new state* of dsql-lint's coverage. Reading it carefully tells us exactly which rules to add next. This is the deliverable the user actually cares about.
- **Skill: superpowers:test-driven-development** for Phases 1–2.
- **Skill: superpowers:verification-before-completion** before Step 6.1.4.
