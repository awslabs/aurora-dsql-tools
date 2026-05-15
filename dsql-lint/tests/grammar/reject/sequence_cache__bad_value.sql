-- production: CreateSeqStmt
-- expectation: reject
-- rule: sequence_cache
-- fix: fixed/sequence_cache__bad_value.sql
CREATE SEQUENCE s CACHE 100;
