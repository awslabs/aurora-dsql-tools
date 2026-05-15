-- production: CreateIndexStmt
-- expectation: reject
-- rule: index_expression
CREATE INDEX ASYNC idx ON t (lower(name));
