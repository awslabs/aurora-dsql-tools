-- production: AlterTableStmt
-- expectation: reject
-- rule: add_column_constraint
ALTER TABLE t ADD COLUMN n INT NOT NULL;
