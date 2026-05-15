-- production: CreateStmt
-- expectation: reject
-- rule: inherits
-- fix: fixed/inherits__basic.sql
CREATE TABLE child (extra INT) INHERITS (parent);
