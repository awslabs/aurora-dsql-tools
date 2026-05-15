-- production: CreateIndexStmt
-- expectation: reject
-- rule: index_partial
CREATE INDEX ASYNC idx ON t(col) WHERE col > 0;
