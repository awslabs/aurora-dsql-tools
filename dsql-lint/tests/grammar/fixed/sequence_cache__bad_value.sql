-- production: CreateSequenceStmt
-- expectation: accept
-- fixes: reject/sequence_cache__bad_value.sql
CREATE SEQUENCE s CACHE 1;
