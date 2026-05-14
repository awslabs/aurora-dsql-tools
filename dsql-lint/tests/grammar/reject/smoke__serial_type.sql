-- production: CreateStmt
-- expectation: reject
-- rule: serial_type
-- fix: fixed/smoke__serial_type.sql
CREATE TABLE smoke_serial (id SERIAL PRIMARY KEY);
