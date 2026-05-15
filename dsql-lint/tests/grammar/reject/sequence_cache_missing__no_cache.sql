-- production: CreateSeqStmt
-- expectation: reject
-- rule: sequence_cache_missing
-- fix: fixed/sequence_cache_missing__no_cache.sql
CREATE SEQUENCE s;
