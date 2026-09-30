# `grammar-diff` maintainer guide

`dsql_grammar.json` is the vendored grammar of what DSQL parses. The
`grammar-diff` binary compares grammar acceptance to dsql-lint acceptance
over a vendored corpus and prints a per-statement diff, so maintainers can
keep the two in sync as DSQL evolves.

## Refreshing after the grammar changes

1. Replace `dsql_grammar.json`. See [`REFRESH.md`](REFRESH.md).
2. Run the diff:

   ```sh
   cargo run --features grammar-diff --release --bin grammar-diff > diff.out
   ```

3. Before triaging, check the load-time warnings (stderr) and the
   `Keyword demotions to IDENT` line at the bottom of the diff. Both
   surface silent drift that masquerades as a flood of `lint-too-lenient`:
   - **Load-time warnings** — three categories, all printed once on stderr
     by `Grammar::load`:
     - `TOP_LEVEL_RULES` entries missing from the grammar, or new `*Stmt`
       rules in the grammar missing from `TOP_LEVEL_RULES`. Either side
       means a whole statement class is being routed to "rejected." Update
       `src/grammar/mod.rs::TOP_LEVEL_RULES`.
     - Non-terminals referenced in any rule but not defined as one. Each
       becomes a non-derivable sink in the recognizer; derivations through
       it silently produce `lint-too-lenient`. Add the missing rule or
       remove the dangling reference.
   - **Keyword demotions** — counts of sqlparser keywords the grammar
     doesn't list (demoted to `IDENT` for recognition). A high or new
     count after a refresh suggests the grammar dropped a keyword.

4. Triage each non-agreement entry:

   | Category | Typical action |
   |---|---|
   | `lint-too-lenient` (lint passes, grammar rejects) | Usually a missing rule. Less commonly: stale `TOP_LEVEL_RULES` (see load-time warnings), or a keyword the grammar dropped (see demotion summary). |
   | `lint-too-strict` (lint flags, grammar accepts) | The grammar is syntactic; many lint rules are semantic (e.g. `ALTER TABLE ADD COLUMN x TEXT NOT NULL` parses but DSQL rejects it). Don't remove a semantic rule just because the grammar accepts the shape. |
   | `parse-error` | Tokenizer mapping gap in `src/grammar/tokenize.rs`, or `accepts` returned `Err` (e.g. zero terminals after the Skip filter — usually means a tokenizer regression). |
   | `agreement` | No action. (Counts only; not printed.) |

## Refreshing the in-tree corpus mirror

The in-tree corpus mirror under `tests/grammar_corpus/in_tree/` is
regenerated from curated arrays in `tests/common/mod.rs`. After editing
those arrays:

```sh
BLESS_MIRROR=1 cargo test --features grammar-diff --test grammar_corpus_mirror_test
```

Without `BLESS_MIRROR`, the test fails on drift.
