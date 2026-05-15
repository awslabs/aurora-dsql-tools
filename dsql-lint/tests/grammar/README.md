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

## Seed coverage

Two rules are excluded from the corpus and covered by dedicated tests:
- `ParseError` — fires on grammatically invalid input; would always fire on `reject/`.
- `MultiDdlTransaction` — multi-statement; corpus is single-statement.

Two `production:` names are allow-listed in `corpus_coverage_test`'s
`KNOWN_GAPS` because DSQL rejects the keyword surface but the EBNF has
no first-class production for the statement:
- `TruncateStmt` — `TRUNCATE` is only a keyword in the EBNF (`reject/truncate__basic.sql`).
- `CreateExtensionStmt` — `CREATE EXTENSION` likewise (`reject/unsupported_statement__create_extension.sql`).

| Rule                       | accept/                          | reject/                           | fixed/   |
|----------------------------|----------------------------------|-----------------------------------|----------|
| serial_type                | smoke__valid_create_table.sql    | smoke__serial_type.sql            | ✅       |
| json_type                  | accept_json_type.sql             | json_type__jsonb.sql              | ✅       |
| array_type                 | accept_array_type.sql            | array_type__text_array.sql        | ❌       |
| foreign_key                | accept_foreign_key.sql           | foreign_key__column_level.sql     | ✅       |
| temp_table                 | accept_temp_table.sql            | temp_table__create_temp.sql       | ✅       |
| partition_by               | accept_partition_by.sql          | partition_by__range.sql           | ✅       |
| inherits                   | accept_inherits.sql              | inherits__basic.sql               | ✅       |
| create_table_as            | accept_create_table_as.sql       | create_table_as__select.sql       | ❌       |
| tablespace                 | accept_tablespace.sql            | tablespace__on_table.sql          | ✅       |
| identity_type              | accept_identity_type.sql         | identity_type__integer.sql        | ✅       |
| identity_cache             | accept_identity_cache.sql        | identity_cache__bad_value.sql     | ✅       |
| identity_cache_missing     | accept_identity_cache.sql        | identity_cache_missing__no_cache.sql | ✅    |
| index_async                | accept_index_async.sql           | index_async__missing.sql          | ✅       |
| index_concurrently         | accept_index_async.sql           | index_concurrently__basic.sql     | ✅       |
| index_using                | accept_index_async.sql           | index_using__btree.sql            | ✅       |
| index_expression           | accept_index_async.sql           | index_expression__lower.sql       | ❌       |
| index_partial              | accept_index_async.sql           | index_partial__where.sql          | ❌       |
| truncate                   | accept_truncate.sql              | truncate__basic.sql               | ❌       |
| sequence_type              | accept_sequence.sql              | sequence_type__integer.sql        | ✅       |
| sequence_cache             | accept_sequence.sql              | sequence_cache__bad_value.sql     | ✅       |
| sequence_cache_missing     | accept_sequence.sql              | sequence_cache_missing__no_cache.sql | ✅    |
| add_column_constraint      | accept_alter_add_column.sql      | add_column_constraint__not_null.sql | ❌     |
| transaction_isolation      | accept_transaction.sql           | transaction_isolation__serializable.sql | ✅  |
| set_transaction            | accept_transaction.sql           | set_transaction__basic.sql        | ❌       |
| unsupported_alter_table_op | accept_alter_table_op.sql        | unsupported_alter_table_op__rls.sql | ❌     |
| unsupported_statement      | accept_unsupported_statement.sql | unsupported_statement__create_extension.sql | ❌ |

✅ = paired `fixed/` golden exists (rule has a `--fix`). ❌ = `FixResult::Unfixable`, no `fixed/` fixture. Source of truth: `FixResult::Fixed(_)` / `FixResult::FixedWithWarning(_)` in `dsql-lint/src/rules/errors.rs`.
