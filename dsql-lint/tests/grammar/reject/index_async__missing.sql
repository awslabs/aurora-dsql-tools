-- production: IndexStmt
-- expectation: reject
-- rule: index_async
-- fix: fixed/index_async__missing.sql
CREATE INDEX idx_foo ON t(col);
