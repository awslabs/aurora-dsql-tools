-- production: CreateSequenceStmt
-- expectation: accept
-- fixes: reject/sequence_type__integer.sql
CREATE SEQUENCE s AS BIGINT CACHE 1;
