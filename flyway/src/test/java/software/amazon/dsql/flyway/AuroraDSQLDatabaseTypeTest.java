/*
 * Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
 * SPDX-License-Identifier: Apache-2.0
 */
package software.amazon.dsql.flyway;

import org.flywaydb.core.api.configuration.FluentConfiguration;
import org.flywaydb.core.internal.jdbc.ExecutionTemplate;
import org.flywaydb.core.internal.sqlscript.SqlScriptExecutorFactory;
import org.junit.jupiter.api.Test;
import org.junit.jupiter.api.DisplayName;
import static org.junit.jupiter.api.Assertions.*;
import static org.junit.jupiter.api.Assumptions.assumeTrue;

/**
 * Unit tests for AuroraDSQLDatabaseType.
 * These tests don't require a database connection.
 */
class AuroraDSQLDatabaseTypeTest {

    private final AuroraDSQLDatabaseType databaseType = new AuroraDSQLDatabaseType();

    @Test
    @DisplayName("Should handle jdbc:aws-dsql:postgresql:// URL")
    void handlesAwsDsqlUrl() {
        assertTrue(databaseType.handlesJDBCUrl(
            "jdbc:aws-dsql:postgresql://abc123.dsql.us-east-1.on.aws/postgres"));
    }

    @Test
    @DisplayName("Should handle jdbc:aws-dsql:postgresql:// URL with port")
    void handlesAwsDsqlUrlWithPort() {
        assertTrue(databaseType.handlesJDBCUrl(
            "jdbc:aws-dsql:postgresql://abc123.dsql.us-east-1.on.aws:5432/postgres"));
    }

    @Test
    @DisplayName("Should handle jdbc:aws-dsql:// URL without postgresql prefix")
    void handlesAwsDsqlUrlWithoutPostgresqlPrefix() {
        assertTrue(databaseType.handlesJDBCUrl(
            "jdbc:aws-dsql://abc123.dsql.us-east-1.on.aws/postgres"));
    }

    @Test
    @DisplayName("Should handle transformed jdbc:postgresql:// URL with DSQL endpoint")
    void handlesTransformedPostgresqlUrlWithDsqlEndpoint() {
        // The DSQL JDBC connector transforms jdbc:aws-dsql:postgresql:// to jdbc:postgresql://
        // but the hostname still contains the DSQL endpoint pattern
        assertTrue(databaseType.handlesJDBCUrl(
            "jdbc:postgresql://abc123.dsql.us-east-1.on.aws:5432/postgres"));
    }

    @Test
    @DisplayName("Should handle transformed URL with different regions")
    void handlesTransformedUrlWithDifferentRegions() {
        assertTrue(databaseType.handlesJDBCUrl(
            "jdbc:postgresql://xyz789.dsql.eu-west-1.on.aws:5432/postgres"));
        assertTrue(databaseType.handlesJDBCUrl(
            "jdbc:postgresql://cluster.dsql.ap-southeast-2.on.aws:5432/postgres"));
    }

    @Test
    @DisplayName("Should handle PrivateLink endpoints")
    void handlesPrivateLinkEndpoints() {
        // PrivateLink endpoints use .dsql-<id> pattern (e.g., .dsql-fnh4)
        assertTrue(databaseType.handlesJDBCUrl(
            "jdbc:postgresql://abc123.dsql-fnh4.us-east-1.on.aws:5432/postgres"));
        assertTrue(databaseType.handlesJDBCUrl(
            "jdbc:aws-dsql:postgresql://abc123.dsql-fnh4.us-east-1.on.aws:5432/postgres"));
    }

    @Test
    @DisplayName("Should NOT handle standard jdbc:postgresql:// URL to localhost")
    void doesNotHandleStandardPostgresqlUrl() {
        assertFalse(databaseType.handlesJDBCUrl(
            "jdbc:postgresql://localhost:5432/mydb"));
    }

    @Test
    @DisplayName("Should NOT handle standard jdbc:postgresql:// URL to RDS")
    void doesNotHandleRdsPostgresqlUrl() {
        assertFalse(databaseType.handlesJDBCUrl(
            "jdbc:postgresql://mydb.abc123.us-east-1.rds.amazonaws.com:5432/mydb"));
    }

    @Test
    @DisplayName("Should NOT handle jdbc:mysql:// URL")
    void doesNotHandleMysqlUrl() {
        assertFalse(databaseType.handlesJDBCUrl(
            "jdbc:mysql://localhost:3306/mydb"));
    }

    @Test
    @DisplayName("Should NOT handle jdbc:oracle:// URL")
    void doesNotHandleOracleUrl() {
        assertFalse(databaseType.handlesJDBCUrl(
            "jdbc:oracle:thin:@localhost:1521:xe"));
    }

    @Test
    @DisplayName("Should have higher priority than PostgreSQL (priority > 0)")
    void hasHigherPriorityThanPostgresql() {
        // PostgreSQL default priority is 0
        assertTrue(databaseType.getPriority() > 0, 
            "DSQL should have higher priority than PostgreSQL to match first");
    }

    @Test
    @DisplayName("Should return 'Aurora DSQL' as database name")
    void returnsCorrectName() {
        assertEquals("Aurora DSQL", databaseType.getName());
    }

    @Test
    @DisplayName("Should return the build's plugin version")
    void returnsPluginVersion() {
        String version = databaseType.getPluginVersion(null);
        assertNotNull(version);
        assertNotEquals("unknown", version, "version.properties was not generated or packaged");

        String expected = System.getProperty("project.version");
        assumeTrue(expected != null && !expected.isBlank(),
            "project.version is only set by the Gradle test task");
        assertEquals(expected, version);
    }

    @Test
    @DisplayName("Should return DSQL JDBC driver class for aws-dsql URLs")
    void returnsCorrectDriverClassForDsqlUrl() {
        String driverClass = databaseType.getDriverClass(
            "jdbc:aws-dsql:postgresql://abc123.dsql.us-east-1.on.aws/postgres", 
            null);
        assertEquals("software.amazon.dsql.jdbc.DSQLConnector", driverClass);
    }

    @Test
    @DisplayName("Should return PostgreSQL driver class for transformed URLs")
    void returnsPostgresDriverClassForTransformedUrl() {
        String driverClass = databaseType.getDriverClass(
            "jdbc:postgresql://abc123.dsql.us-east-1.on.aws:5432/postgres",
            null);
        assertEquals("org.postgresql.Driver", driverClass);
    }

    @Test
    @DisplayName("Should NOT handle a DSQL pattern outside the host")
    void doesNotHandleDsqlPatternOutsideHost() {
        // The DSQL pattern must match the host only: a ".dsql." in the database name or a query
        // parameter must not hijack an ordinary PostgreSQL URL (priority 1 would win selection).
        assertFalse(databaseType.handlesJDBCUrl("jdbc:postgresql://localhost:5432/app.dsql.production"));
        assertFalse(databaseType.handlesJDBCUrl("jdbc:postgresql://localhost:5432/app?ApplicationName=.dsql."));
    }

    @Test
    @DisplayName("Should wrap the transactional template with the OCC-retry decorator")
    void wrapsTransactionalTemplateWithOccRetryDecorator() {
        // The commit-wrapping seam must return our OCC-retry decorator, otherwise a commit-time
        // DSQL conflict is never retried. Base PostgreSQL just news up a TransactionalExecutionTemplate
        // holding the connection (no I/O), so a null connection is fine for the type check.
        ExecutionTemplate template = databaseType.createTransactionalExecutionTemplate(null, true);
        assertTrue(template instanceof AuroraDSQLExecutionTemplate);
    }

    @Test
    @DisplayName("Should wrap the SQL-script executor factory for async-index waiting")
    void wrapsSqlScriptExecutorFactoryForAsyncIndexWait() {
        // The SQL-script executor seam must return our wrapper so CREATE INDEX ASYNC waits.
        // Base PostgreSQL builds the delegate factory lazily (no I/O until an executor is made),
        // so null collaborators are fine for the type check.
        SqlScriptExecutorFactory factory = databaseType.createSqlScriptExecutorFactory(null, null, null);
        assertTrue(factory instanceof AuroraDSQLSqlScriptExecutorFactory);
    }

    @Test
    @DisplayName("maxRetryDelayMillis converts seconds to millis")
    void maxRetryDelayConvertsSecondsToMillis() {
        // Guards the seconds->millis conversion: dropping the *1000 would make backoff 1000x too short.
        assertEquals(30_000L, AuroraDSQLDatabaseType.maxRetryDelayMillis(30));
    }

    @Test
    @DisplayName("maxRetryDelayMillis floors at the minimum")
    void maxRetryDelayFloorsAtMinimum() {
        // A zero or negative configured cap must not disable backoff; it floors at 100ms.
        assertEquals(100L, AuroraDSQLDatabaseType.maxRetryDelayMillis(0));
        assertEquals(100L, AuroraDSQLDatabaseType.maxRetryDelayMillis(-5));
    }

    @Test
    @DisplayName("resolveOccRetryKnobs falls back to defaults when config is null")
    void resolveKnobsFallsBackToDefaultsWhenConfigNull() {
        assertArrayEquals(new int[]{0, 5}, AuroraDSQLDatabaseType.resolveOccRetryKnobs(null));
    }

    @Test
    @DisplayName("resolveOccRetryKnobs reads configured extension values")
    void resolveKnobsReadsConfiguredExtensionValues() {
        // Drives the real resolution path: FluentConfiguration auto-registers the extension via
        // ServiceLoader, so this proves configured values actually reach the transactional template.
        FluentConfiguration config = new FluentConfiguration();
        AuroraDSQLConfigurationExtension ext =
                config.getPluginRegister().getPlugin(AuroraDSQLConfigurationExtension.class);
        ext.setOccMaxRetries(3);
        ext.setOccMaxRetryDelaySeconds(10);
        assertArrayEquals(new int[]{3, 10}, AuroraDSQLDatabaseType.resolveOccRetryKnobs(config));
    }

    @Test
    @DisplayName("resolveOccRetryKnobs clamps negative retries to zero")
    void resolveKnobsClampsNegativeRetriesToZero() {
        FluentConfiguration config = new FluentConfiguration();
        config.getPluginRegister().getPlugin(AuroraDSQLConfigurationExtension.class).setOccMaxRetries(-1);
        assertEquals(0, AuroraDSQLDatabaseType.resolveOccRetryKnobs(config)[0]);
    }

    @Test
    @DisplayName("resolveOccRetryKnobs clamps excessive retries to the connector max")
    void resolveKnobsClampsExcessiveRetriesToConnectorMax() {
        // OCCRetryConfig.build() rejects maxRetries > 100; the clamp keeps an oversized config value
        // from failing the migration when building the retry config.
        FluentConfiguration config = new FluentConfiguration();
        config.getPluginRegister().getPlugin(AuroraDSQLConfigurationExtension.class).setOccMaxRetries(500);
        assertEquals(100, AuroraDSQLDatabaseType.resolveOccRetryKnobs(config)[0]);
    }
}
