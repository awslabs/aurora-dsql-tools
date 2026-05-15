-- production: IndexStmt
-- expectation: accept
-- fixes: reject/index_concurrently__basic.sql
CREATE INDEX ASYNC idx ON t(col);
