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
 * <p>Skips {@code SET ROLE} restoration (DSQL uses IAM authentication) and bypasses
 * advisory locks (DSQL uses optimistic concurrency control).
 */
public class AuroraDSQLConnection extends PostgreSQLConnection {

    private static final Logger LOG = Logger.getLogger(AuroraDSQLConnection.class.getName());

    public AuroraDSQLConnection(AuroraDSQLDatabase database, Connection connection) {
        super(database, connection);
    }

    @Override
    protected void doRestoreOriginalState() throws SQLException {
        LOG.fine("Skipping SET ROLE restoration (not supported by Aurora DSQL)");
    }

    @Override
    public Schema getSchema(String name) {
        return new AuroraDSQLSchema(jdbcTemplate, (AuroraDSQLDatabase) database, name);
    }

    @Override
    public <T> T lock(Table table, Callable<T> callable) {
        LOG.fine("Executing without advisory lock (not supported by Aurora DSQL)");
        try {
            T result = callable.call();
            reconcileDuplicateHistoryRows(table);
            return result;
        } catch (SQLException e) {
            throw new FlywaySqlException("Unable to execute migration", e);
        } catch (RuntimeException e) {
            throw e;
        } catch (Exception e) {
            throw new RuntimeException("Unable to execute migration", e);
        }
    }

    /**
     * Removes duplicate schema-history rows left by an OCC retry.
     *
     * <p>DSQL keeps the migration's schema-history insert on the auto-commit main connection, so it
     * commits before the migration transaction's own commit. When that commit loses the OCC race and
     * {@link AuroraDSQLExecutionTemplate} replays the migration, a second history row is written for the
     * same version — leaving the earlier attempt's row orphaned (a lower {@code installed_rank}).
     * This keeps the highest {@code installed_rank} per version and deletes the lower duplicates.
     *
     * <p>Best-effort and idempotent: a no-op when there are no duplicates, and never fails the
     * migration if cleanup itself errors. Rows with no version (baseline / schema markers) are left
     * untouched.
     */
    private void reconcileDuplicateHistoryRows(Table table) {
        String deleteDuplicates =
                "DELETE FROM " + table + " h"
                        + " WHERE h.\"version\" IS NOT NULL"
                        + " AND h.\"installed_rank\" < ("
                        + "SELECT MAX(h2.\"installed_rank\") FROM " + table + " h2"
                        + " WHERE h2.\"version\" = h.\"version\")";
        try {
            // lock() also wraps the history-table create/drop; skip those (e.g. after clean drops
            // the table) so the DELETE does not run against a missing table. The main connection is
            // already auto-commit, so the DELETE commits on its own.
            if (!table.exists()) {
                return;
            }
            jdbcTemplate.execute(deleteDuplicates);
        } catch (Exception e) {
            // Best-effort: never fail an already-succeeded migration. Broad catch because
            // table.exists() rethrows its SQLException unchecked.
            LOG.warning("Could not reconcile duplicate Aurora DSQL schema-history rows: " + e.getMessage());
        }
    }
}
