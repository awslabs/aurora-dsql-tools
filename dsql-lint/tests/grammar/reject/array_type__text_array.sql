-- production: columnDef
-- expectation: reject
-- rule: array_type
CREATE TABLE t (id INT, tags TEXT[]);
