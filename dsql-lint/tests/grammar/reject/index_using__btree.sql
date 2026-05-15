-- production: CreateIndexStmt
-- expectation: reject
-- rule: index_using
-- fix: fixed/index_using__btree.sql
CREATE INDEX ASYNC idx ON t USING btree(col);
