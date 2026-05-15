-- production: CreateSeqStmt
-- expectation: accept
-- rule: sequence_type
CREATE SEQUENCE s AS BIGINT CACHE 1;
