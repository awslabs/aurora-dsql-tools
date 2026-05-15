-- production: ColumnDef
-- expectation: accept
-- fixes: reject/foreign_key__column_level.sql
CREATE TABLE t (
  id INT,
  cid INT  
);
