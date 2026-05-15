-- production: columnDef
-- expectation: accept
-- fixes: reject/json_type__jsonb.sql
CREATE TABLE t (
  id INT,
  data JSON  
);
