-- production: columnDef
-- expectation: reject
-- rule: json_type
-- fix: fixed/json_type__jsonb.sql
CREATE TABLE t (id INT, data JSONB);
