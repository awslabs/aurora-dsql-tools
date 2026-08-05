/*
 * Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
 * SPDX-License-Identifier: Apache-2.0
 */
package software.amazon.dsql.flyway;

import org.flywaydb.core.api.configuration.Configuration;
import org.flywaydb.core.internal.jdbc.Results;
import org.flywaydb.core.internal.sqlscript.SqlScript;
import org.flywaydb.core.internal.sqlscript.SqlScriptExecutor;
import org.flywaydb.core.internal.sqlscript.SqlScriptExecutorFactory;
import org.junit.jupiter.api.Test;

import java.sql.Connection;
import java.util.List;

import static java.util.Collections.emptyList;
import static org.assertj.core.api.Assertions.assertThat;

class AuroraDSQLSqlScriptExecutorFactoryTest {

    private static final SqlScriptExecutor STUB_EXECUTOR = new SqlScriptExecutor() {
        @Override public List<Results> execute(SqlScript sqlScript, Configuration configuration) {
            return emptyList();
        }
    };

    @Test
    void wrapsDelegateExecutorWithDsqlExecutor() {
        SqlScriptExecutorFactory delegate =
                (connection, undo, batch, outputQueryResults) -> STUB_EXECUTOR;
        AuroraDSQLSqlScriptExecutorFactory factory = new AuroraDSQLSqlScriptExecutorFactory(delegate);

        SqlScriptExecutor executor = factory.createSqlScriptExecutor(null, false, false, false);

        assertThat(executor).isInstanceOf(AuroraDSQLSqlScriptExecutor.class);
    }
}
