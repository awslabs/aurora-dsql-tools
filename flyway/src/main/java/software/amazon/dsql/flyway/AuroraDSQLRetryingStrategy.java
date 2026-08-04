/*
 * Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
 * SPDX-License-Identifier: Apache-2.0
 */
package software.amazon.dsql.flyway;

import org.flywaydb.core.internal.database.DatabaseExecutionStrategy;
import org.flywaydb.core.internal.util.SqlCallable;

import java.sql.SQLException;
import java.util.logging.Logger;

/**
 * Retries statements that fail with DSQL's optimistic concurrency conflict ({@code OC001}, SQLSTATE
 * {@code 40001}), which DSQL documents as retryable.
 *
 * <p>Every DDL statement bumps the cluster's schema catalog version. Flyway reuses one connection
 * for a whole {@code migrate} run, so staleness accumulates across migrations and a run can conflict
 * with its own earlier DDL - no concurrent deployment needed. Without a retry that transient failure
 * is permanent: migrations which cannot run in a transaction are recorded as failed rather than
 * rolled back, so later runs need {@code flyway repair}. Flyway takes the same approach for
 * CockroachDB in {@code CockroachDBRetryingStrategy}.</p>
 *
 * <p>A retry re-runs only the failed migration's script, from the top, so DSQL migrations should be
 * idempotent (for example {@code CREATE TABLE IF NOT EXISTS}).</p>
 */
public class AuroraDSQLRetryingStrategy implements DatabaseExecutionStrategy {

    private static final Logger LOG = Logger.getLogger(AuroraDSQLRetryingStrategy.class.getName());

    /** SQLSTATE for serialization failure. DSQL reports OC001 catalog conflicts with this state. */
    private static final String OCC_CONFLICT_SQL_STATE = "40001";

    private static final int MAX_RETRIES = 10;

    /**
     * Backoff grows from this value up to {@link #MAX_BACKOFF_MS}. DSQL discovers catalog changes
     * reactively, so a conflict can take a moment to clear - unlike Flyway's CockroachDB strategy,
     * which retries in a tight loop, we wait between attempts.
     */
    private static final long INITIAL_BACKOFF_MS = 100;

    private static final long MAX_BACKOFF_MS = 4_000;

    private final int maxRetries;
    private final long initialBackoffMs;
    private final long maxBackoffMs;

    public AuroraDSQLRetryingStrategy() {
        this(MAX_RETRIES, INITIAL_BACKOFF_MS, MAX_BACKOFF_MS);
    }

    // Visible for testing, so tests can exercise the retry loop without waiting on real backoff.
    AuroraDSQLRetryingStrategy(int maxRetries, long initialBackoffMs, long maxBackoffMs) {
        this.maxRetries = maxRetries;
        this.initialBackoffMs = initialBackoffMs;
        this.maxBackoffMs = maxBackoffMs;
    }

    @Override
    public <T> T execute(SqlCallable<T> callable) throws SQLException {
        long backoffMs = initialBackoffMs;

        for (int attempt = 0; ; attempt++) {
            try {
                return callable.call();
            } catch (SQLException e) {
                // Anything other than a conflict, or a conflict we are out of retries for, is the
                // caller's problem. Rethrow unchanged so Flyway reports the original SQLSTATE.
                if (attempt >= maxRetries || !OCC_CONFLICT_SQL_STATE.equals(e.getSQLState())) {
                    throw e;
                }

                LOG.info("Aurora DSQL optimistic concurrency conflict (attempt " + (attempt + 1)
                        + " of " + maxRetries + "), retrying in " + backoffMs + "ms: "
                        + e.getMessage());

                sleepBeforeRetry(backoffMs, e);
                backoffMs = Math.min(backoffMs * 2, maxBackoffMs);
            }
        }
    }

    private void sleepBeforeRetry(long backoffMs, SQLException cause) throws SQLException {
        try {
            Thread.sleep(backoffMs);
        } catch (InterruptedException ie) {
            // Preserve the interrupt for callers up the stack and give up on retrying. This method
            // may only throw SQLException, so surface the conflict that prompted the retry.
            Thread.currentThread().interrupt();
            throw cause;
        }
    }
}
