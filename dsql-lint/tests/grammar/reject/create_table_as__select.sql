-- production: CreateStmt
-- expectation: reject
-- rule: create_table_as
CREATE TABLE t AS SELECT 1 AS id;
