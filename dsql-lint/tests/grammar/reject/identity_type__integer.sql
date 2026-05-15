-- production: columnDef
-- expectation: reject
-- rule: identity_type
-- fix: fixed/identity_type__integer.sql
CREATE TABLE t (id INTEGER GENERATED ALWAYS AS IDENTITY);
