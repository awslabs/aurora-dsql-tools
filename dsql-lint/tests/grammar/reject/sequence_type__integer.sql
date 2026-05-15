-- production: CreateSequenceStmt
-- expectation: reject
-- rule: sequence_type
-- fix: fixed/sequence_type__integer.sql
CREATE SEQUENCE s AS INTEGER;
