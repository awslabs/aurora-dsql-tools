-- production: columnDef
-- expectation: reject
-- rule: identity_cache
-- fix: fixed/identity_cache__bad_value.sql
CREATE TABLE t (id BIGINT GENERATED ALWAYS AS IDENTITY (CACHE 100));
