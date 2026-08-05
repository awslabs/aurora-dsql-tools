/*
 * Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
 * SPDX-License-Identifier: Apache-2.0
 */
package software.amazon.dsql.flyway;

import org.junit.jupiter.api.Test;

import java.sql.SQLException;

import static org.assertj.core.api.Assertions.assertThat;

class AuroraDSQLOccErrorsTest {

    @Test
    void detectsOccSqlStates() {
        assertThat(AuroraDSQLOccErrors.isOccError(new SQLException("data conflict", "OC000"))).isTrue();
        assertThat(AuroraDSQLOccErrors.isOccError(new SQLException("catalog conflict", "OC001"))).isTrue();
        assertThat(AuroraDSQLOccErrors.isOccError(new SQLException("serialization", "40001"))).isTrue();
    }

    @Test
    void ignoresNonOccSqlStates() {
        assertThat(AuroraDSQLOccErrors.isOccError(new SQLException("syntax error", "42601"))).isFalse();
        assertThat(AuroraDSQLOccErrors.isOccError(new SQLException("no state", (String) null))).isFalse();
    }

    @Test
    void detectsOccInCauseChain() {
        SQLException root = new SQLException("catalog conflict", "OC001");
        SQLException wrapper = new SQLException("wrapped", "XX000");
        wrapper.initCause(root);
        assertThat(AuroraDSQLOccErrors.isOccError(wrapper)).isTrue();
    }

    @Test
    void detectsOccInNextExceptionChain() {
        SQLException first = new SQLException("first", "XX000");
        SQLException next = new SQLException("serialization", "40001");
        first.setNextException(next);
        assertThat(AuroraDSQLOccErrors.isOccError(first)).isTrue();
    }

    @Test
    void nullIsNotOcc() {
        assertThat(AuroraDSQLOccErrors.isOccError(null)).isFalse();
    }
}
