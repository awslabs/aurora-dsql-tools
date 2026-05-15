-- production: CreateStmt
-- expectation: accept
-- fixes: reject/partition_by__range.sql
CREATE TABLE t (
  id INT,
  d DATE  
);
