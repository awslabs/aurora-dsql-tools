-- production: TransactionStmt
-- expectation: reject
-- rule: transaction_isolation
-- fix: fixed/transaction_isolation__serializable.sql
BEGIN ISOLATION LEVEL SERIALIZABLE;
