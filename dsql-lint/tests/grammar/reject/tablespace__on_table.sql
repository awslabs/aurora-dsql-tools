-- production: CreateStmt
-- expectation: reject
-- rule: tablespace
-- fix: fixed/tablespace__on_table.sql
CREATE TABLE t (id INT) TABLESPACE my_space;
