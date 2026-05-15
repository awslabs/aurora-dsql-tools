-- production: AlterTableStmt
-- expectation: reject
-- rule: unsupported_alter_table_op
ALTER TABLE t ENABLE ROW LEVEL SECURITY;
