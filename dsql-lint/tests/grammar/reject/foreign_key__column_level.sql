-- production: columnDef
-- expectation: reject
-- rule: foreign_key
-- fix: fixed/foreign_key__column_level.sql
CREATE TABLE t (id INT, cid INT REFERENCES c(id));
