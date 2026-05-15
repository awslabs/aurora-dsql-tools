-- production: CreateSequenceStmt
-- expectation: accept
-- fixes: reject/sequence_cache_missing__no_cache.sql
CREATE SEQUENCE s CACHE 1;
