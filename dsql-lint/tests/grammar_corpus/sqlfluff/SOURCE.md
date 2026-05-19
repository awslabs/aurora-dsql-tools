# sqlfluff Postgres dialect fixtures

Vendored for the `grammar-diff` maintainer tool. Provides broad PG syntax
coverage on the grammar side.

- **Upstream:** https://github.com/sqlfluff/sqlfluff
- **Path:** `test/fixtures/dialects/postgres/`
- **Pinned commit:** `92c8eef84df478bcb82ee67c8384a133f82c6dfb`
- **License:** MIT (see `LICENSE.md`)

## Refresh

Run from `aurora-dsql-tools/dsql-lint`:

```sh
tmp=$(mktemp -d)
git clone --depth 1 --filter=blob:none --sparse \
    https://github.com/sqlfluff/sqlfluff "$tmp"
( cd "$tmp" && git sparse-checkout set test/fixtures/dialects/postgres )

find tests/grammar_corpus/sqlfluff -maxdepth 1 -name '*.sql' -delete
cp "$tmp/test/fixtures/dialects/postgres/"*.sql tests/grammar_corpus/sqlfluff/
cp "$tmp/LICENSE.md" tests/grammar_corpus/sqlfluff/LICENSE.md

# Record the new pinned commit in this file:
( cd "$tmp" && git rev-parse HEAD )
```
