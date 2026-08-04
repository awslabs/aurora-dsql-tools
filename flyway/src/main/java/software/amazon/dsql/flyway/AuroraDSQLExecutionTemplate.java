/*
 * Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
 * SPDX-License-Identifier: Apache-2.0
 */
package software.amazon.dsql.flyway;

import org.flywaydb.core.api.FlywayException;
import org.flywaydb.core.internal.jdbc.ExecutionTemplate;
import software.amazon.dsql.jdbc.OCCRetryConfig;

import java.util.concurrent.Callable;
import java.util.concurrent.ThreadLocalRandom;
import java.util.logging.Logger;

/**
 * Retries a whole migration transaction on an Aurora DSQL OCC conflict.
 *
 * <p>DSQL detects conflicts at commit time ({@code OC000}/{@code OC001}/{@code 40001}). Flyway
 * runs each migration inside an {@link ExecutionTemplate} whose {@code execute} calls
 * {@code connection.commit()} <em>after</em> the callback, so a conflict surfaces at the
 * transaction boundary — not inside a single statement. This decorator wraps the delegate
 * transactional template and retries the entire callback (statements + commit) when the
 * failure carries an OCC SQLSTATE. A conflicting transaction is rolled back before the retry, so
 * nothing was committed to replay; SQL migrations are re-parsed and re-run cleanly. (A Java
 * migration with external side effects would re-execute those side effects on each attempt.)
 *
 * <p>Replaying the callback also replays Flyway's in-memory bookkeeping: a migration that commits
 * only after a retry can appear more than once in the reported {@code MigrateResult}. The
 * migration's own statements roll back with the failed commit, but the schema-history insert runs
 * on the auto-commit main connection and survives, so a retried migration leaves a duplicate
 * history row that {@link AuroraDSQLConnection} reconciles afterward.
 *
 * <p>This wraps every transactional commit for the DSQL type, not just migrations (e.g.
 * schema-history table creation), so OCC conflicts on those commits are retried here too.
 *
 * <p>The retry knobs (count, base/max delay, multiplier, jitter) come from the Aurora DSQL JDBC
 * connector's {@link OCCRetryConfig} so the contract stays in sync with the connector rather than
 * being re-declared here; the {@code dsql} configuration supplies the count and cap. Retry is
 * opt-in: {@code occMaxRetries} defaults to 0, so this is a no-op unless a positive count is set.
 */
class AuroraDSQLExecutionTemplate implements ExecutionTemplate {

    private static final Logger LOG = Logger.getLogger(AuroraDSQLExecutionTemplate.class.getName());

    private final ExecutionTemplate delegate;
    private final OCCRetryConfig retryConfig;

    AuroraDSQLExecutionTemplate(ExecutionTemplate delegate, OCCRetryConfig retryConfig) {
        this.delegate = delegate;
        this.retryConfig = retryConfig;
    }

    @Override
    public <T> T execute(Callable<T> callback) {
        int maxRetries = retryConfig.getMaxRetries();
        int attempt = 0;
        while (true) {
            try {
                return delegate.execute(callback);
            } catch (RuntimeException e) {
                if (!AuroraDSQLOccErrors.isOccError(e) || attempt >= maxRetries) {
                    throw e;
                }
                sleep(backoff(attempt));
                attempt++;
                LOG.info("Retrying migration transaction after Aurora DSQL OCC conflict (attempt "
                        + attempt + " of " + maxRetries + ")");
            }
        }
    }

    // Mirrors the connector's OCCRetry.calculateBackoff (package-private there, so it cannot be
    // reused): exponential base delay capped at maxDelayMs, plus up to jitterFactor of that delay.
    private long backoff(int attempt) {
        int exponent = Math.min(attempt, 31);
        double delay = Math.min(retryConfig.getBaseDelayMs() * Math.pow(retryConfig.getMultiplier(), exponent),
                retryConfig.getMaxDelayMs());
        double jitter = delay * ThreadLocalRandom.current().nextDouble() * retryConfig.getJitterFactor();
        return (long) (delay + jitter);
    }

    private void sleep(long millis) {
        try {
            Thread.sleep(millis);
        } catch (InterruptedException ie) {
            Thread.currentThread().interrupt();
            throw new FlywayException("Aurora DSQL OCC retry interrupted", ie);
        }
    }
}
