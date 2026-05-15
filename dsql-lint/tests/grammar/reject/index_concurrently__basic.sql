-- production: IndexStmt
-- expectation: reject
-- rule: index_concurrently
-- fix: fixed/index_concurrently__basic.sql
CREATE INDEX CONCURRENTLY idx ON t(col);
