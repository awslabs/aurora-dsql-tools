-- production: TransactionStmt
-- expectation: reject
-- rule: set_transaction
SET TRANSACTION ISOLATION LEVEL SERIALIZABLE;
