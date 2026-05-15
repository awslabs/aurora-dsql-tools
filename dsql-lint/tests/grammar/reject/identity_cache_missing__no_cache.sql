-- production: columnDef
-- expectation: reject
-- rule: identity_cache_missing
-- fix: fixed/identity_cache_missing__no_cache.sql
CREATE TABLE t (id BIGINT GENERATED ALWAYS AS IDENTITY);
