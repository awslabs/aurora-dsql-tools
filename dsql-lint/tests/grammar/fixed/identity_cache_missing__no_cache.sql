-- production: ColumnDef
-- expectation: accept
-- fixes: reject/identity_cache_missing__no_cache.sql
CREATE TABLE t (
  id BIGINT GENERATED ALWAYS AS IDENTITY ( CACHE 1 )  
);
