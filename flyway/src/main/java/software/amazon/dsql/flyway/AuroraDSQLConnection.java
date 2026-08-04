/*
 * Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
 * SPDX-License-Identifier: Apache-2.0
 */
package software.amazon.dsql.flyway;

import org.flywaydb.core.internal.database.base.Schema;
import org.flywaydb.core.internal.database.base.Table;
import org.flywaydb.core.internal.exception.FlywaySqlException;
import org.flywaydb.database.postgresql.PostgreSQLConnection;

import java.sql.Connection;
import java.sql.SQLException;
import java.util.concurrent.Callable;
import java.util.logging.Logger;

/**
 * Aurora DSQL connection implementation for Flyway.
 *
 * <p>Overrides PostgreSQL connection behavior: skips SET ROLE (DSQL uses IAM auth),
 * bypasses advisory locks (DSQL uses OCC, with conflicts retried by
 * {@link AuroraDSQLRetryingStrategy}), and returns DSQL-compatible schemas.</p>
 */
public class AuroraDSQLConnection extends PostgreSQLConnection {

    private static final Logger LOG = Logger.getLogger(AuroraDSQLConnection.class.getName());

    public AuroraDSQLConnection(AuroraDSQLDatabase database, Connection connection) {
        super(database, connection);
    }

    /**
     * Skips SET ROLE restoration - DSQL uses IAM authentication where role is fixed.
     */
    @Override
    protected void doRestoreOriginalState() throws SQLException {
        LOG.fine("Skipping SET ROLE restoration (not supported by Aurora DSQL)");
    }

    @Override
    public Schema getSchema(String name) {
        return new AuroraDSQLSchema(jdbcTemplate, (AuroraDSQLDatabase) database, name);
    }

    /**
     * Executes the callable without advisory locks (not supported by DSQL).
     * <p>Optimistic concurrency conflicts are retried by {@link AuroraDSQLRetryingStrategy}, not
     * here. Do not add retry logic to this method: Flyway records a failed migration in
     * {@code flyway_schema_history} inside this callable and rethrows
     * {@code FlywayMigrateException}, so by the time an exception reaches this frame the failure is
     * already persisted and retrying would not clear it.</p>
     */
    @Override
    public <T> T lock(Table table, Callable<T> callable) {
        LOG.fine("Executing without advisory lock (not supported by Aurora DSQL)");
        try {
            return callable.call();
        } catch (SQLException e) {
            // Not the migration-failure path. Flyway wraps those in FlywayMigrateException,
            // a RuntimeException, which the next catch rethrows untouched.
            throw new FlywaySqlException("Unable to execute migration", e);
        } catch (RuntimeException e) {
            throw e;
        } catch (Exception e) {
            throw new RuntimeException("Unable to execute migration", e);
        }
    }
}
