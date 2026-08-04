/*
 * Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
 * SPDX-License-Identifier: Apache-2.0
 */
package software.amazon.dsql.flyway;

import org.flywaydb.core.internal.util.SqlCallable;
import org.junit.jupiter.api.DisplayName;
import org.junit.jupiter.api.Test;

import java.lang.reflect.Proxy;
import java.sql.Connection;
import java.sql.SQLException;

import static org.junit.jupiter.api.Assertions.*;

/**
 * Unit tests for AuroraDSQLRetryingStrategy.
 *
 * <p>These tests use short backoffs so the retry loop can be exercised without real waits.</p>
 */
class AuroraDSQLRetryingStrategyTest {

    private static final String OCC_CONFLICT = "40001";
    private static final int MAX_RETRIES = 3;

    /** Fails with the given SQLSTATE the first {@code failures} times, then returns "ok". */
    private static final class CountingCallable implements SqlCallable<String> {
        private final int failures;
        private final String sqlState;
        private int calls;

        CountingCallable(int failures, String sqlState) {
            this.failures = failures;
            this.sqlState = sqlState;
        }

        @Override
        public String call() throws SQLException {
            calls++;
            if (calls <= failures) {
                throw new SQLException("schema has been updated by another transaction", sqlState);
            }
            return "ok";
        }
    }

    private AuroraDSQLRetryingStrategy strategy() {
        return new AuroraDSQLRetryingStrategy(MAX_RETRIES, 1, 2);
    }

    @Test
    @DisplayName("Should not retry when the callable succeeds")
    void succeedsWithoutRetry() throws SQLException {
        CountingCallable callable = new CountingCallable(0, OCC_CONFLICT);

        assertEquals("ok", strategy().execute(callable));
        assertEquals(1, callable.calls, "a successful call should not be retried");
    }

    @Test
    @DisplayName("Should retry on SQLSTATE 40001 until it succeeds")
    void retriesOnOccConflict() throws SQLException {
        CountingCallable callable = new CountingCallable(2, OCC_CONFLICT);

        assertEquals("ok", strategy().execute(callable));
        assertEquals(3, callable.calls, "should retry twice, then succeed");
    }

    @Test
    @DisplayName("Should propagate a non-40001 SQLException without retrying")
    void doesNotRetryOtherSqlStates() {
        // 42P01 = undefined_table, a deterministic error that retrying cannot fix.
        CountingCallable callable = new CountingCallable(1, "42P01");

        SQLException thrown = assertThrows(SQLException.class, () -> strategy().execute(callable));

        assertEquals("42P01", thrown.getSQLState());
        assertEquals(1, callable.calls, "non-conflict errors should fail fast");
    }

    @Test
    @DisplayName("Should give up after the retry limit and rethrow the conflict")
    void stopsRetryingAtLimit() {
        CountingCallable callable = new CountingCallable(Integer.MAX_VALUE, OCC_CONFLICT);

        SQLException thrown = assertThrows(SQLException.class, () -> strategy().execute(callable));

        assertEquals(OCC_CONFLICT, thrown.getSQLState(),
            "the original conflict should surface, so Flyway reports the real SQLSTATE");
        assertEquals(MAX_RETRIES + 1, callable.calls,
            "should make one initial attempt plus MAX_RETRIES retries");
    }

    @Test
    @DisplayName("Should propagate a null SQLSTATE without retrying")
    void doesNotRetryNullSqlState() {
        SqlCallable<String> callable = () -> {
            throw new SQLException("connection reset");
        };

        assertThrows(SQLException.class, () -> strategy().execute(callable));
    }

    @Test
    @DisplayName("createExecutionStrategy() should return the retrying strategy")
    void databaseTypeReturnsRetryingStrategy() {
        AuroraDSQLDatabaseType databaseType = new AuroraDSQLDatabaseType();

        // The strategy never touches the connection, so a no-op proxy is enough here.
        Connection connection = (Connection) Proxy.newProxyInstance(
            Connection.class.getClassLoader(),
            new Class<?>[] { Connection.class },
            (proxy, method, args) -> null);

        assertInstanceOf(AuroraDSQLRetryingStrategy.class,
            databaseType.createExecutionStrategy(connection));
    }

    @Test
    @DisplayName("createExecutionStrategy(null) should fall back to the default strategy")
    void databaseTypeHandlesNullConnection() {
        AuroraDSQLDatabaseType databaseType = new AuroraDSQLDatabaseType();

        assertNotNull(databaseType.createExecutionStrategy(null));
        assertFalse(databaseType.createExecutionStrategy(null) instanceof AuroraDSQLRetryingStrategy,
            "a null connection has nothing to retry against");
    }
}
