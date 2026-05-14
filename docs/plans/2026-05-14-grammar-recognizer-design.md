# Grammar Recognizer Design — dsql-lint

Date: 2026-05-14
Status: **Deferred.** The corpus-based design at
[`2026-05-14-grammar-integration-design.md`](2026-05-14-grammar-integration-design.md)
was selected instead. This document is retained as a reference for the
recognizer approach, which may be revisited if the corpus approach proves
insufficient (see "Future work" in the corpus design).

## Problem

`dsql_grammar.ebnf` describes what DSQL's parser actually accepts. dsql-lint's
rules in [`src/rules/errors.rs`](../../dsql-lint/src/rules/errors.rs) are
hand-written against the `sqlparser-dsql` AST. The two encode the same
DSQL-specific knowledge in two languages, and can drift:

1. **Grammar tightens.** A production narrows; a previously-flagged construct
   becomes outright invalid. The rule's message may now be misleading, or a
   new violation slips through silently.
2. **Grammar relaxes.** A production widens; a construct dsql-lint still
   flags is now valid in DSQL. The rule blocks legitimate SQL. (Recent
   precedent: PR #53 removed the `json_type` rule because DSQL gained
   native JSON support.)
3. **Coverage gaps.** Things the grammar rejects today that dsql-lint also
   doesn't catch. Currently invisible.
4. **Suggested fixes are wrong.** A rule's suggestion or `--fix` output
   produces SQL the grammar itself doesn't accept. Today there is no
   programmatic check that fix output is grammar-valid.

The grammar is the source of truth and will keep evolving. We want CI to
fail when dsql-lint and the grammar disagree, in a way that points the
maintainer at the specific rule that needs updating.

## Constraints

- The EBNF file is **read-only**: it comes from upstream. We cannot
  restructure productions, eliminate left-recursion at the source, or add
  precedence/associativity annotations.
- The shipped binary's runtime path stays unchanged. No grammar at lint
  time, no second parser to maintain, no new runtime dependency. The
  recognizer is test-only.
- Hand-rolled rules keep ownership of messages, suggestions, AST mutation,
  and warning text. Auto-generation of diagnostics is a non-goal.

## Approach: grammar as a test-time conformance oracle

The grammar is loaded once per test run into a `Grammar` value that answers
one question: `grammar.accepts(sql) -> bool`. Every lint rule declares two
SQL probes — a violation and (optionally) the SQL the rule's fix produces —
and a single conformance test asserts the rules and the grammar agree on
each.

Runtime is untouched. Rule suggestions/warnings/fixes stay hand-rolled.
The grammar becomes load-bearing for *correctness over time*: if it relaxes
or tightens, tests break and force rules to follow.

### Architecture

```
dsql-lint/
├── src/                                 # unchanged at runtime
│   ├── lint.rs
│   └── rules/errors.rs                  # hand-rolled rules
├── tests/
│   ├── grammar/                         # NEW: test-only module
│   │   ├── mod.rs                       # public test API
│   │   ├── ebnf.rs                      # parses dsql_grammar.ebnf → Grammar
│   │   └── recognizer.rs                # constructs chumsky Parser, runs accepts()
│   └── grammar_conformance_test.rs      # NEW: rule probes + assertions
└── dsql_grammar.ebnf                    # already exists, read-only
```

Three responsibilities, cleanly separated:

- **`ebnf.rs`** parses the EBNF text into a typed `Grammar` value
  (`HashMap<RuleName, Production>` where `Production` is
  `Sequence | Choice | Repetition | Optional | Terminal | NonTerminal`).
  The EBNF format is small and regular; no parser generator needed for
  this layer.
- **`recognizer.rs`** walks the `Grammar` and constructs a
  [`chumsky`](https://crates.io/crates/chumsky) parser for the start
  rule, then exposes `Grammar::accepts(&self, sql: &str) -> bool`. Two
  carve-outs (see below) handle the EBNF's quirks.
- **`grammar_conformance_test.rs`** declares per-rule probes and asserts
  the conformance contract.

### Why `chumsky` and not a hand-written recognizer or `peg`

- **`peg`** encodes its grammar in a Rust macro at compile time. We'd have
  to translate `dsql_grammar.ebnf` into hand-written `peg!` rules and
  keep them in sync — defeating the point.
- **Hand-written** is viable (~600 lines) but reproduces machinery
  `chumsky` already provides (backtracking, choice, error recovery,
  EOF handling).
- **`chumsky`** is combinator-based: parsers are values built
  programmatically from the EBNF AST loaded at test startup. The grammar
  file stays the source of truth — when upstream regenerates it, our
  test infrastructure picks up the change with no Rust file to update.

`chumsky` is added as a `[dev-dependencies]` entry. It does not affect
`cargo install` users or the prebuilt npm/PyPI binaries.

### Carve-outs for grammar quirks

Two transformations apply while building the chumsky parser. Both are
DSQL-specific in the sense that they encode SQL knowledge the EBNF leaves
implicit; everything else is generic.

1. **Left-recursive expression productions.** Productions of the form
   `Foo = Foo OP Bar | atom` (the EBNF's `a_expr` and `b_expr`) are
   detected by shape and built using `chumsky`'s `pratt` combinator with
   a hardcoded Postgres operator-precedence table (~30 entries).
2. **Undefined terminals.** The EBNF references `identifier`,
   `string_literal`, `integer_constant`, `float_constant`, `operator`,
   `binary_string`, `hex_string` without defining them. Each maps to a
   small, fixed `chumsky` parser following Postgres lexical rules. This
   is the only place lexical knowledge is encoded.

Estimated size: `ebnf.rs` ~150 lines, `recognizer.rs` ~250 lines.
Test-only, no new runtime dependency.

## The conformance contract

Each rule declares its probes co-located with the conformance test:

```rust
pub struct RuleProbes {
    pub rule: LintRule,
    /// SQL the rule must flag. Grammar must reject it.
    pub violation: &'static str,
    /// SQL after the rule's suggested fix is applied. Grammar must accept it,
    /// and lint_sql must emit no diagnostics on it.
    /// `None` for unfixable rules — only the violation/grammar-rejects
    /// assertion runs.
    pub fix: Option<&'static str>,
}
```

Example:

```rust
RuleProbes {
    rule: LintRule::ForeignKey,
    violation: "CREATE TABLE t (id INT, other_id INT REFERENCES other(id));",
    fix:       Some("CREATE TABLE t (id INT, other_id INT);"),
}
```

The conformance test iterates every `LintRule` variant via the existing
`strum::EnumIter` derive ([`lint.rs:34`](../../dsql-lint/src/lint.rs#L34))
and asserts five clauses per rule:

1. A `RuleProbes` entry exists for the variant. (Catches: someone added
   `LintRule::Foo` but forgot probes.)
2. `grammar.accepts(violation) == false`. (Catches: grammar relaxed —
   rule may be obsolete.)
3. `lint_sql(violation)` reports a diagnostic with `rule == self.rule`.
   (Catches: rule logic regressed.)
4. If `fix.is_some()`: `grammar.accepts(fix) == true`. (Catches: our
   suggested fix isn't actually DSQL-valid.)
5. If `fix.is_some()`: `lint_sql(fix)` reports no diagnostic with
   `rule == self.rule`. (Catches: rule fires on its own suggestion.)

Two rules need special handling and are exempted from the standard shape:

- `LintRule::ParseError` — by definition fires on input the grammar also
  rejects. No probe; covered by existing tests.
- `LintRule::MultiDdlTransaction` — multi-statement input, not a
  single-statement syntax restriction. Has a dedicated handwritten test.

Three rules out of 28 with custom handling is acceptable; forcing every
rule into one shape is what makes test frameworks rot.

### Failure messages

Each contract clause emits a tailored failure message that says what the
failure *means*, not just what mismatched. Example for clause 2:

```
LintRule::ForeignKey: grammar accepts the violation probe.
  Probe SQL: CREATE TABLE t (id INT, other_id INT REFERENCES other(id));
  This usually means the grammar relaxed and this rule may be obsolete.
  Action: verify FK is now supported in DSQL, then remove the rule and
  its probe.
```

### One test, batched failures

A naive design would be one `#[test]` per rule (28 tests). Instead, all
five clauses across all rules run in a single `#[test]` that collects
failures into a `Vec<String>` and panics with the joined message. Reason:
when the grammar relaxes, *one* upstream change can invalidate many
probes at once. Reporting them all in one CI run lets the maintainer see
the full impact instead of fixing them one at a time.

A separate, cheap `#[test] every_rule_has_probes` runs without loading the
grammar, so the common "I added a rule, forgot probes" mistake fails fast
with a precise message.

## Drift scenarios

| Drift                             | What CI does                                                                  |
| --------------------------------- | ----------------------------------------------------------------------------- |
| Grammar tightens (new restriction)| New rule added in same PR with probes. Conformance test passes.               |
| Grammar relaxes (e.g. FK allowed) | Clause 2 fails on `LintRule::ForeignKey`. Maintainer removes rule and probe.  |
| Rule's suggested fix is invalid   | Clause 4 fails. Maintainer fixes the rule's transformation.                   |
| Rule fires on its own fix         | Clause 5 fails. Maintainer narrows the rule's match condition.                |
| Rule logic regressed              | Clause 3 fails. Maintainer fixes the rule.                                    |

The FK relaxation walkthrough specifically: grammar gains an FK production
→ recognizer accepts the violation probe → clause 2 fails for
`LintRule::ForeignKey` → CI tells the maintainer to delete the rule.
One PR, mechanical.

## What's out of scope

- **No runtime grammar parser.** The recognizer never runs in the shipped
  binary. `grammar.accepts()` is a test-only oracle.
- **No auto-generated rules.** Diagnostics, suggestions, and fix code stay
  hand-written in `errors.rs`.
- **No replacement of `sqlparser-dsql`.** The existing rule engine and
  AST walker stay.
- **No grammar editing.** The EBNF file is read-only; carve-outs handle
  its quirks at recognizer-build time.
- **No probe escape hatches.** A `skip_grammar_check` flag would let the
  oracle quietly stop being authoritative. If the recognizer is wrong on a
  real probe, the recognizer gets fixed.

## Rollout plan

Three PRs. Each is independently reviewable and revertable.

### PR 1 — Grammar loader and recognizer

Lands `tests/grammar/{mod,ebnf,recognizer}.rs` plus `chumsky` as a
dev-dep. One self-test exercising a handful of known-valid and
known-invalid statements:

```rust
#[test]
fn grammar_loads_and_accepts_known_valid_sql() {
    let g = Grammar::load_from_file("dsql_grammar.ebnf");
    assert!(g.accepts("CREATE TABLE t (id BIGINT PRIMARY KEY);"));
    assert!(g.accepts("CREATE INDEX ASYNC idx ON t(c);"));
    assert!(!g.accepts("CREATE TABLE t (id SERIAL PRIMARY KEY);"));
    assert!(!g.accepts("CREATE INDEX idx ON t(c);"));  // missing ASYNC
}
```

No rule logic touched, no risk to runtime path. If the recognizer is
subtly wrong, we discover it on a bounded surface before any rule
depends on it.

### PR 2 — Conformance harness with probes for all current rules

Lands `tests/grammar_conformance_test.rs` and the `PROBES` table. Two
dedicated tests for `ParseError` and `MultiDdlTransaction`. Expected to
surface 2–4 small bugs in `errors.rs` (stale messages, fix output that
doesn't actually parse) — those get fixed in the same PR if cheap, split
out if not.

### PR 3 — Build-time fact extraction (optional)

A small `build.rs` parses the EBNF and emits `OUT_DIR/grammar_facts.rs`
with three constants:

```rust
pub const ALLOWED_CACHE_VALUES: &[i64] = &[1, 65536];
pub const ALLOWED_ISOLATION_LEVELS: &[&str] = &["REPEATABLE READ"];
pub const INDEX_REQUIRES_ASYNC: bool = true;
```

`errors.rs` imports these and replaces the corresponding hardcoded
literals at [`errors.rs:698`](../../dsql-lint/src/rules/errors.rs#L698)
and [`errors.rs:1015`](../../dsql-lint/src/rules/errors.rs#L1015). The
conformance test in PR 2 already catches grammar drift; this PR is a
small ergonomic win where the grammar happens to encode a clean
numeric/list fact.

**Defer or skip if** the three replacements feel forced, or `build.rs`
complicates the cross-compile setup at `.github/workflows/` for the
prebuilt npm/PyPI binaries. Land PRs 1 and 2 first, evaluate, then
decide.

## Open questions

- **Probe co-location.** Probes currently live in
  `tests/grammar_conformance_test.rs`. An alternative is to put each
  probe next to its rule code in `errors.rs` (under `#[cfg(test)]`).
  The single-file location is simpler for the harness; the co-located
  variant is harder to forget. Pick during PR 2 review.
- **Recognizer correctness budget.** If a clause-2 failure turns out to
  be a recognizer bug rather than real grammar drift, how loud is the
  signal? Initial answer: treat the recognizer as production-grade test
  infrastructure — bug reports get treated like any other test bug.
  No `skip_grammar_check` escape hatch.
- **PR 3 trigger.** Build-time extraction is opt-in. Concrete criterion
  for landing it: the conformance test has been green for 4+ weeks and
  at least one of the three target literals has been changed by hand
  during that window.
