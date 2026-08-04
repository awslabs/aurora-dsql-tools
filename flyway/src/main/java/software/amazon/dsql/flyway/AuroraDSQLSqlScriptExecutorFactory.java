/*
 * Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
 * SPDX-License-Identifier: Apache-2.0
 */
package software.amazon.dsql.flyway;

import org.flywaydb.core.internal.sqlscript.SqlScriptExecutor;
import org.flywaydb.core.internal.sqlscript.SqlScriptExecutorFactory;

import java.sql.Connection;

/**
 * Wraps the default {@link SqlScriptExecutorFactory} so each SQL-script executor can block on
 * Aurora DSQL {@code CREATE INDEX ASYNC} builds when {@code flyway.dsql.awaitAsyncIndexes} is
 * enabled (see {@link AuroraDSQLSqlScriptExecutor}). The factory always wraps; the blocking itself is
 * opt-in and off by default.
 *
 * <p>Kept separate from the executor so it can be unit-tested with a fake delegate factory: the
 * base factory closes over a {@code JdbcConnectionFactory} whose constructor opens a live
 * connection, so the real one cannot be built with nulls.
 */
class AuroraDSQLSqlScriptExecutorFactory implements SqlScriptExecutorFactory {

    private final SqlScriptExecutorFactory delegate;

    AuroraDSQLSqlScriptExecutorFactory(SqlScriptExecutorFactory delegate) {
        this.delegate = delegate;
    }

    @Override
    public SqlScriptExecutor createSqlScriptExecutor(Connection connection, boolean undo,
                                                     boolean batch, boolean outputQueryResults) {
        SqlScriptExecutor delegateExecutor =
                delegate.createSqlScriptExecutor(connection, undo, batch, outputQueryResults);
        return new AuroraDSQLSqlScriptExecutor(delegateExecutor, connection);
    }
}
