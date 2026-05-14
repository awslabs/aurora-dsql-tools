# Grammar Integration Design — dsql-lint

Date: 2026-05-14
Status: Accepted (implementation not started). Supersedes
[`2026-05-14-grammar-recognizer-design.md`](2026-05-14-grammar-recognizer-design.md).

## Problem

`dsql_grammar.ebnf` describes what DSQL's parser actually accepts. dsql-lint's
rules are hand-written against `sqlparser-rs`. The two can drift:

1. Grammar **tightens** (used to permit X, no longer does): dsql-lint may
   silently accept X, missing a real incompatibility.
2. Grammar **relaxes** (newly permits X): dsql-lint may still flag X as an
   error, blocking valid SQL. Recent example: PR #53 removed the `json_type`
   rule because DSQL gained native JSON support.
3. **Coverage gaps**: things the grammar rejects today that dsql-lint also
   doesn't catch. We want these surfaced, not hidden.
4. **Fixture drift**: existing test SQL no longer matches what the grammar
   says is valid.

The grammar will keep evolving. CI must fail when dsql-lint and the grammar
disagree.

## Non-goals

- **No runtime grammar parser.** Building one from the EBNF as written is a
  multi-week effort: ambiguous left-recursion with no precedence annotations,
  no lexer rules, no error-recovery story. The shipped binary stays
  sqlparser-rs + hand-written rules.
- **No auto-generated rules.** Diagnostics are human-authored — the grammar
  is the oracle, not the source of error messages.
- **No replacement of sqlparser-rs.** The existing rule engine stays.

## Approach: corpus as the contract

The EBNF does not drive CI directly. Instead it drives a **labeled SQL
corpus** that *is* the contract CI checks.

```
dsql-lint/tests/grammar/
  accept/   # grammar accepts → dsql-lint must NOT emit ERROR
  reject/   # grammar rejects → dsql-lint MUST emit ERROR
  fixed/    # paired post-fix SQL → grammar accepts AND dsql-lint clean
```

Each fixture has a header naming the grammar production it exercises.
The optional `fix:` key points at a paired post-fix file:

```sql
-- production: CreateStmt, INHERITS clause
-- expectation: reject
-- fix: fixed/create_table_inherits.sql
CREATE TABLE child () INHERITS (parent);
```

The paired `fixed/create_table_inherits.sql` is the **golden output** of
running `dsql-lint --fix` on the `reject/` fixture. It carries its own
header:

```sql
-- production: CreateStmt
-- expectation: accept
-- fixes: reject/create_table_inherits.sql
CREATE TABLE child ();
```

A new test, `tests/grammar_oracle.rs`, walks all three directories and runs
`dsql_lint::lint_sql` on each fixture. Failures are explicit:

```
grammar/reject/create_table_inherits.sql exercises 'INHERITS' but
dsql-lint emitted no ERROR. Either add a rule, or move this fixture
to accept/ if INHERITS is now valid.
```

### Why this covers all four drift types

| Drift                           | What happens in CI                                          |
| ------------------------------- | ----------------------------------------------------------- |
| Grammar tightens                | Author adds `reject/` fixture in same PR; CI fails until a rule exists. |
| Grammar relaxes                 | Author moves fixture from `reject/` to `accept/`; CI fails until rule is removed/loosened. |
| Coverage gap                    | Author adds a `reject/` fixture, no rule catches it; test fails. Tagged with `// TODO(coverage)` and `#[ignore]` on the specific case until a rule lands. |
| Fixture drift                   | Existing rule fixtures cross-reference productions; coverage scan flags references to renamed/removed productions. |
| Suggested fix is broken         | For each `reject/` fixture with `fix:`, CI runs the rule's `--fix` over the input and diffs the output against `fixed/`. Mismatch (rule changed its output) or post-fix lint errors (fix produces SQL that re-fires a rule) both fail CI. |

## Workflow

### 1. Grammar PR checklist

`CONTRIBUTING.md` gains a section: when you push a new `dsql_grammar.ebnf`,
the same PR must update `tests/grammar/accept/`, `tests/grammar/reject/`,
or `tests/grammar/fixed/` for any production whose semantics changed. If
the change touches a rule that has a `--fix`, regenerate the paired
`fixed/` golden by running `cargo test corpus_contract_test --
--bless` (or whatever command the test exposes for golden updates).

### 2. Diff helper (optional, advisory)

`dsql-lint/scripts/grammar_diff.sh`:

```bash
git diff main -- dsql_grammar.ebnf | grep -E '^[+-][A-Za-z]'
```

Lists changed production names. Not a parser — a reminder to update the
corpus. Output pasted into the PR body.

### 3. CI gates

Two tests in `tests/grammar_oracle.rs`:

- **`corpus_contract_test`** — walks `accept/`, `reject/`, and `fixed/`,
  runs `lint_sql` on each. Fails if an `accept/` fixture produces an ERROR,
  a `reject/` fixture produces no ERROR, or a `fixed/` fixture produces any
  ERROR. For each `reject/` fixture with a `fix:` header, additionally:
  (a) runs `dsql-lint --fix` on the input and asserts byte-equality with
  the paired `fixed/` file (golden-output check); (b) asserts the
  `fixed/` file's `fixes:` back-reference matches. A `--bless` flag
  rewrites the `fixed/` file from the rule's actual output for easy
  updates. Failure messages cite the file path, production tag, observed
  diagnostics, and the diff against the golden when applicable.

- **`corpus_coverage_test`** — extracts production names from the EBNF
  (regex `^[A-Za-z][A-Za-z0-9_]* =`), extracts production tags from corpus
  headers, prints productions with zero corpus coverage. Initially
  informational. Once we have meaningful coverage (target: every production
  named in `errors.rs` rules), promote uncovered productions of interest to
  hard failures.

A renamed production that the corpus still references surfaces here as a
warning, catching fixture drift.

## What's explicitly out of scope

- No EBNF Rust parser. The file is treated as text + a production-name index.
- No runtime grammar fallback in the binary.
- No fixture auto-generation from the EBNF.
- No precedence/associativity annotations layered onto the grammar.

## Open questions / future work

- **Coverage ratcheting.** Once the corpus stabilises, decide a threshold
  (e.g. all productions referenced by an `errors.rs` rule must have ≥1
  corpus fixture) and promote `corpus_coverage_test` from informational to
  failing.
- **Header schema.** Lock down the header keys (`production:`,
  `expectation:`, optional `rule:`, optional `fix:` / `fixes:`) and reject
  malformed headers in the corpus test, so fixtures stay machine-readable.
- **`unsupported_statement` / `parse_error` interaction.** Decide whether a
  `reject/` fixture flagged only as `parse_error` (sqlparser couldn't parse
  it at all) counts as a passing assertion or a coverage gap.

## Implementation phases

1. Land the corpus directory layout (`accept/`, `reject/`, `fixed/`), header
   schema, and `corpus_contract_test`. Seed with one `accept/` and one
   `reject/` fixture per existing rule in `errors.rs`, plus a `fixed/`
   fixture for every rule that has a suggestion or `--fix`.
2. Add `corpus_coverage_test` (informational mode) and the `grammar_diff.sh`
   helper.
3. Update `CONTRIBUTING.md` with the grammar-PR checklist (including the
   "if your rule has a fix, add a `fixed/` fixture" requirement).
4. Iterate on coverage; eventually promote `corpus_coverage_test` to a hard
   failure for productions tied to existing rules.

## Future work: grammar-as-recognizer

A more ambitious alternative — building a chumsky-based recognizer from
the EBNF and asserting `grammar.accepts(sql)` directly — was considered
and deferred. See
[`2026-05-14-grammar-recognizer-design.md`](2026-05-14-grammar-recognizer-design.md)
for the full design. Its main advantage over this plan is that it
verifies fix output against the grammar itself, not just against
dsql-lint's own rules. Revisit if (a) the corpus approach proves too
contributor-dependent, or (b) we hit a class of bug where dsql-lint and
the grammar disagree on what a fix should produce.
