-- production: ColumnDef
-- expectation: accept
-- fixes: reject/identity_type__integer.sql
CREATE TABLE t (
  id BIGINT GENERATED ALWAYS AS IDENTITY ( CACHE 1 )  
);
