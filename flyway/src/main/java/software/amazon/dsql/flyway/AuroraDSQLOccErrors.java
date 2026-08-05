/*
 * Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
 * SPDX-License-Identifier: Apache-2.0
 */
package software.amazon.dsql.flyway;

import software.amazon.dsql.jdbc.OCCRetry;

import java.sql.SQLException;

/**
 * Detects Aurora DSQL optimistic-concurrency-control (OCC) conflicts.
 *
 * <p>The SQLSTATE contract ({@code OC000} data conflict, {@code OC001} catalog/schema conflict,
 * {@code 40001} serialization) is owned by the connector's {@link OCCRetry#isOCCError(SQLException)}
 * and delegated to here so the two never drift. What this class adds is Flyway-specific: the
 * connector inspects a single raw {@link SQLException}, but at the transaction-template layer the
 * conflict arrives wrapped in a {@code FlywaySqlException} (not a {@code SQLException}), so we walk
 * the cause / next-exception chain and delegate each {@link SQLException} node to the connector.
 */
final class AuroraDSQLOccErrors {

    private AuroraDSQLOccErrors() {
    }

    /**
     * Returns true if the given exception, or anything in its cause / next-exception
     * chain, is an Aurora DSQL OCC conflict.
     */
    static boolean isOccError(Throwable throwable) {
        Throwable current = throwable;
        while (current != null) {
            if (current instanceof SQLException) {
                SQLException sqlException = (SQLException) current;
                if (OCCRetry.isOCCError(sqlException)) {
                    return true;
                }
                SQLException next = sqlException.getNextException();
                if (next != null && isOccError(next)) {
                    return true;
                }
            }
            current = current.getCause();
        }
        return false;
    }
}
