#!/usr/bin/env bash
# Refresh the vendored Postgres regression-test corpus used by the grammar
# oracle drift test. Pulls a curated DDL-focused subset from postgres/postgres
# at a pinned commit/tag, strips psql metacommands, and writes the result to
# `dsql-lint/tests/grammar_oracle/pg_corpus/`.
#
# Run from the repo root (or anywhere — script computes its own paths):
#   ./dsql-lint/scripts/refresh_pg_corpus.sh
#
# Override the upstream ref:
#   PG_REF=REL_17_STABLE ./dsql-lint/scripts/refresh_pg_corpus.sh
set -euo pipefail

PG_REF="${PG_REF:-REL_16_STABLE}"

# Files we vendor — DDL-focused. SELECT/INSERT/UPDATE/DELETE/EXPLAIN files are
# omitted because the recognizer can't parse those statement classes today
# (chumsky backtracks pathologically; see drift.rs::RECOGNIZER_BLOCKLIST).
# plpgsql / replication / optimizer torture tests are also out of scope —
# they exercise features DSQL doesn't support and don't surface lint gaps.
FILES=(
    create_table.sql
    create_index.sql
    create_view.sql
    create_schema.sql
    create_type.sql
    alter_table.sql
    alter_generic.sql
    truncate.sql
    transactions.sql
    sequence.sql
    constraints.sql
    drop_if_exists.sql
)

repo_root="$(cd "$(dirname "$0")/../.." && pwd)"
out_dir="$repo_root/dsql-lint/tests/grammar_oracle/pg_corpus"
tmp_dir="$(mktemp -d)"
trap 'rm -rf "$tmp_dir"' EXIT

echo "Cloning postgres/postgres@$PG_REF (sparse) into $tmp_dir ..."
git clone \
    --depth 1 \
    --branch "$PG_REF" \
    --filter=blob:none \
    --sparse \
    https://github.com/postgres/postgres "$tmp_dir/pg" >/dev/null
git -C "$tmp_dir/pg" sparse-checkout set src/test/regress/sql/ >/dev/null

mkdir -p "$out_dir"
# Wipe stale files but keep the README and .gitignore (if any).
find "$out_dir" -maxdepth 1 -name '*.sql' -delete

pinned_commit="$(git -C "$tmp_dir/pg" rev-parse HEAD)"

echo "Vendoring ${#FILES[@]} file(s) ..."
for f in "${FILES[@]}"; do
    src="$tmp_dir/pg/src/test/regress/sql/$f"
    if [[ ! -f "$src" ]]; then
        echo "  SKIP $f (not in upstream at $PG_REF)" >&2
        continue
    fi
    cp "$src" "$out_dir/$f"
    echo "  $f"
done

# Record the pinned ref so the test can read it back, and so refresh diffs
# are easy to spot in code review.
{
    echo "# Pinned upstream ref for the vendored Postgres regression corpus."
    echo "# Refresh with: ./dsql-lint/scripts/refresh_pg_corpus.sh"
    echo "ref=$PG_REF"
    echo "commit=$pinned_commit"
} > "$out_dir/PG_REF"

echo "Done. Pinned commit: $pinned_commit"
echo "Run \`cargo test -p dsql-lint --test grammar_oracle\` next; new drift is expected and goes into EXPECTED_DRIFT."
