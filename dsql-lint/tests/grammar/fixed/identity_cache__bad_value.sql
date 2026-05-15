-- production: columnDef
-- expectation: accept
-- fixes: reject/identity_cache__bad_value.sql
CREATE TABLE t (
  id BIGINT GENERATED ALWAYS AS IDENTITY ( CACHE 1 )  
);
