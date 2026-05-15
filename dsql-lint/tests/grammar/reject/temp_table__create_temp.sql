-- production: CreateStmt
-- expectation: reject
-- rule: temp_table
-- fix: fixed/temp_table__create_temp.sql
CREATE TEMP TABLE t (id INT);
