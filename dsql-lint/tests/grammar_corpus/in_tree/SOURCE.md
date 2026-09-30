# In-tree corpus mirror

Mechanically generated from curated arrays in `tests/common/mod.rs`:
`SUPPORTED_TYPES`, `CLEAN_STATEMENTS`, `FALSE_POSITIVE_CASES`,
`ADDITIONAL_ERROR_CASES`, `ADDITIONAL_FALSE_POSITIVES`. Provides per-rule
positive/negative coverage on the lint side.

After editing the source arrays:

```sh
BLESS_MIRROR=1 cargo test --features grammar-diff --test grammar_corpus_mirror_test
```

Without `BLESS_MIRROR`, the test fails on drift.
