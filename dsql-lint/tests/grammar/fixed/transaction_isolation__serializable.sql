-- production: TransactionStmt
-- expectation: accept
-- fixes: reject/transaction_isolation__serializable.sql
BEGIN ISOLATION LEVEL REPEATABLE READ;
