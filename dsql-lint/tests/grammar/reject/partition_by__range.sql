-- production: CreateStmt
-- expectation: reject
-- rule: partition_by
-- fix: fixed/partition_by__range.sql
CREATE TABLE t (id INT, d DATE) PARTITION BY RANGE (d);
