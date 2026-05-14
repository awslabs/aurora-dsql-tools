-- production: CreateStmt
-- expectation: reject
-- rule: serial_type
CREATE TABLE smoke_serial (id SERIAL PRIMARY KEY);
