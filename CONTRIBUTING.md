# Contributing Guidelines

Thank you for your interest in contributing to our project. Whether it's a bug report, new feature, correction, or additional
documentation, we greatly value feedback and contributions from our community.

Please read through this document before submitting any issues or pull requests to ensure we have all the necessary
information to effectively respond to your bug report or contribution.


## Reporting Bugs/Feature Requests

We welcome you to use the GitHub issue tracker to report bugs or suggest features.

When filing an issue, please check existing open, or recently closed, issues to make sure somebody else hasn't already
reported the issue. Please try to include as much information as you can. Details like these are incredibly useful:

* A reproducible test case or series of steps
* The version of our code being used
* Any modifications you've made relevant to the bug
* Anything unusual about your environment or deployment


## Contributing via Pull Requests
Contributions via pull requests are much appreciated. Before sending us a pull request, please ensure that:

1. You are working against the latest source on the *main* branch.
2. You check existing open, and recently merged, pull requests to make sure someone else hasn't addressed the problem already.
3. You open an issue to discuss any significant work - we would hate for your time to be wasted.

To send us a pull request, please:

1. Fork the repository.
2. Modify the source; please focus on the specific change you are contributing. If you also reformat all the code, it will be hard for us to focus on your change.
3. Ensure local tests pass.
4. Commit to your fork using clear commit messages.
5. Send us a pull request, answering any default questions in the pull request interface.
6. Pay attention to any automated CI failures reported in the pull request, and stay involved in the conversation.

GitHub provides additional document on [forking a repository](https://help.github.com/articles/fork-a-repo/) and
[creating a pull request](https://help.github.com/articles/creating-a-pull-request/).


## Finding contributions to work on
Looking at the existing issues is a great way to find something to contribute on. As our projects, by default, use the default GitHub issue labels (enhancement/bug/duplicate/help wanted/invalid/question/wontfix), looking at any 'help wanted' issues is a great place to start.


## Code of Conduct
This project has adopted the [Amazon Open Source Code of Conduct](https://aws.github.io/code-of-conduct).
For more information see the [Code of Conduct FAQ](https://aws.github.io/code-of-conduct-faq) or contact
opensource-codeofconduct@amazon.com with any additional questions or comments.


## Security issue notifications
If you discover a potential security issue in this project we ask that you notify AWS/Amazon Security via our [vulnerability reporting page](http://aws.amazon.com/security/vulnerability-reporting/). Please do **not** create a public github issue.


## Licensing

See the [LICENSE](LICENSE) file for our project's licensing. We will ask you to confirm the licensing of your contribution.


## Grammar oracle (dsql-lint)

`dsql_grammar.json` at the repo root is the source of truth for what
DSQL's parser accepts. It is a vendored JSON description of the DSQL
grammar; refresh it by replacing the file with an updated copy and
re-running the oracle tests.

The oracle (`dsql-lint/tests/grammar_oracle/grammar.rs`) loads that
JSON and answers `accepts(sql) -> bool` via input-driven derivation:
tokenize the SQL, then for each `*Stmt` rule check whether some
derivation consumes all the tokens. Memoizes on `(rule, cursor)` so
complexity stays polynomial. Handles left recursion via the standard
`A = β α*` rewrite, and grammar `repetition` fields for list shapes.

When the grammar JSON changes — e.g., DSQL relaxes a restriction or
adds support for a statement form — the oracle's verdicts change
automatically. No translation step.

Cluster tests remain authoritative for correctness; the oracle is a
second signal that surfaces drift to maintainers.

When `dsql_lint_agrees_with_grammar` fails, the message names the
disagreement kind:

- **`GrammarRejectsLintQuiet`** — grammar rejects, dsql-lint silent.
  Often: dsql-lint is missing a rule (add one in
  `dsql-lint/src/rules/errors.rs`).
- **`LintFlagsGrammarAccepts`** — dsql-lint flags it, grammar accepts.
  Either the grammar relaxed (remove or loosen the rule) or dsql-lint
  is over-flagging.

### The burndown list

Tolerated disagreements from the curated dsql-lint corpus live in
`EXPECTED_DRIFT` in `dsql-lint/tests/grammar_oracle/drift.rs`. Each
entry is `(sql, reason)`:

- **`DriftReason::MissingDsqlLintRule`** — grammar correctly rejects;
  dsql-lint should grow a rule. **This is the actionable list.** When
  picking work, filter `EXPECTED_DRIFT` by this variant and pick the
  next entry.
- **`DriftReason::RecognizerHole`** — the input-driven oracle doesn't
  follow this grammar shape correctly. Today these include the
  `SignedIconst` stub (used by `IDENTITY (CACHE …)` and `SEQUENCE …
  CACHE …`), bare `BEGIN;`, `COPY t FROM STDIN`, and SELECT/UPDATE/
  DELETE shapes that need fuller derivation support. Recognizer work,
  not rule work — don't chase a "missing dsql-lint rule" interpretation
  for these entries.
- **`DriftReason::SemanticOnly`** — grammar permits-by-design; the
  restriction is semantic and dsql-lint catches it separately. The
  grammar's `Typename → identifier` path matches any identifier as a
  type name, so SERIAL/JSONB/array-type rejection lives in dsql-lint,
  not the grammar.

The list fails CI on stale entries, so it shrinks naturally as fixes
land — every removal corresponds to a new rule, an oracle fix, or a
grammar update. Adding a new entry requires a comment explaining why
it's tolerated.

### Postgres regression-test corpus

The curated dsql-lint corpus (`tests/integration_test.rs`,
`tests/common/mod.rs`) is what dsql-lint already checks — a corpus
selected that way can't surface what dsql-lint *doesn't* check. To
find real coverage gaps the oracle also walks a vendored subset of
the upstream PostgreSQL regression-test SQL under
`dsql-lint/tests/grammar_oracle/pg_corpus/`.

Drift on this larger corpus is **report-only** today: the agreement
test prints a count + breakdown (LintFlagsGrammarAccepts vs
GrammarRejectsLintQuiet) but does not fail CI. The
GrammarRejectsLintQuiet entries are the rule-gap candidates. To see
the full burndown surface:

```bash
cargo test -p dsql-lint --test grammar_oracle pg_corpus_drift_report -- --nocapture
```

Refresh the vendored corpus by re-running the upstream sync:

```bash
./dsql-lint/scripts/refresh_pg_corpus.sh
# or pin a different upstream ref:
PG_REF=REL_17_STABLE ./dsql-lint/scripts/refresh_pg_corpus.sh
```

Statements that `lint_sql` flags as `ParseError` are skipped:
dsql-lint can't lint what it can't parse, so they aren't drift
signal — the PG corpus contains intentionally-broken SQL marked
`-- fail` plus exotic constructs `sqlparser-dsql` doesn't model.
