# Grammar corpus

Each fixture pairs a small SQL probe with what dsql-lint should do with it.
The corpus is the contract CI checks: dsql-lint must lint `accept/` clean,
must fire on `reject/`, and `--fix` output must match the paired `fixed/`
golden byte-for-byte (regenerate with `BLESS=1 cargo test -p dsql-lint
--test grammar_oracle`).

## Layout

- `accept/` — grammar accepts; dsql-lint must NOT emit a diagnostic
- `reject/` — grammar rejects; dsql-lint must emit at least one diagnostic
- `fixed/` — golden output of `dsql-lint --fix` over a paired `reject/` fixture

## Header schema

Every fixture starts with a SQL-comment header:

- `production:` — required. Name of the grammar production exercised.
- `expectation:` — required. `accept` or `reject`. Must match directory.
- `rule:` — optional. Snake-case `LintRule` variant the fixture targets.
- `fix:` (reject only) — optional. Relative path to paired `fixed/` file.
- `fixes:` (fixed only) — required. Back-reference to the `reject/` fixture.

## Naming

- `accept/<rule>__<label>.sql` — boundary-case SQL the rule must NOT flag.
  Many rules share an accept fixture (e.g. several type rules use
  `CREATE TABLE t (id BIGINT PRIMARY KEY);`).
- `reject/<rule>__<label>.sql` — SQL the rule MUST flag, optionally with a
  `fix:` header pointing at a paired golden.
- `fixed/<rule>__<label>.sql` — golden output of `dsql-lint --fix` over its
  paired `reject/` fixture. Only present when the rule has a `FixResult::Fixed`
  or `FixResult::FixedWithWarning` path in `src/rules/errors.rs`.

## Excluded from the corpus

Two `LintRule` variants do not get fixtures and are covered by dedicated tests
elsewhere:

- `ParseError` — fires on grammatically invalid input; would always fire on `reject/`.
- `MultiDdlTransaction` — multi-statement; corpus is single-statement.

## Known gaps in `corpus_coverage_test`

Two `production:` names are allow-listed in `corpus_coverage_test`'s
`KNOWN_GAPS` because DSQL rejects the keyword surface but the EBNF has no
first-class production for the statement:

- `TruncateStmt` — `TRUNCATE` is only a keyword in the EBNF (`reject/truncate__basic.sql`).
- `CreateExtensionStmt` — `CREATE EXTENSION` likewise (`reject/unsupported_statement__create_extension.sql`).
