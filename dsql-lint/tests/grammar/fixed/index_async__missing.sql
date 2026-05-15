-- production: CreateIndexStmt
-- expectation: accept
-- fixes: reject/index_async__missing.sql
CREATE INDEX ASYNC idx_foo ON t(col);
