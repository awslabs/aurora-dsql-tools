-- production: IndexStmt
-- expectation: accept
-- fixes: reject/index_using__btree.sql
CREATE INDEX ASYNC idx ON t(col);
