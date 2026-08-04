/*
 * Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
 * SPDX-License-Identifier: Apache-2.0
 */
package software.amazon.dsql.flyway;

import org.flywaydb.core.Flyway;
import org.flywaydb.core.api.output.MigrateResult;
import org.junit.jupiter.api.*;

import java.sql.Connection;
import java.sql.DriverManager;
import java.sql.PreparedStatement;
import java.sql.ResultSet;
import java.sql.Statement;
import java.util.UUID;

import static org.junit.jupiter.api.Assertions.*;

/**
 * Integration tests for waiting on asynchronous unique index builds.
 *
 * <p>Configure via {@code DSQL_CLUSTER_ENDPOINT}. Run with {@code ./gradlew integrationTest}.</p>
 */
@TestMethodOrder(MethodOrderer.OrderAnnotation.class)
@TestInstance(TestInstance.Lifecycle.PER_CLASS)
class AuroraDSQLAsyncIndexIntegrationTest {

    private String jdbcUrl;

    @BeforeAll
    void setUp() {
        String clusterEndpoint = System.getProperty("dsql.cluster.endpoint",
                System.getenv("DSQL_CLUSTER_ENDPOINT"));

        if (clusterEndpoint == null || clusterEndpoint.isEmpty()) {
            throw new IllegalStateException("DSQL_CLUSTER_ENDPOINT must be set");
        }

        jdbcUrl = String.format("jdbc:aws-dsql:postgresql://%s:5432/postgres", clusterEndpoint);
    }

    private String createSchema() throws Exception {
        String schema = "async_idx_" + UUID.randomUUID().toString().replace("-", "").substring(0, 8);
        try (Connection conn = DriverManager.getConnection(jdbcUrl, "admin", null);
             Statement stmt = conn.createStatement()) {
            stmt.execute("CREATE SCHEMA " + schema);
        }
        return schema;
    }

    private void dropSchema(String schema) {
        try (Connection conn = DriverManager.getConnection(jdbcUrl, "admin", null);
             Statement stmt = conn.createStatement()) {
            stmt.execute("DROP SCHEMA IF EXISTS " + schema + " CASCADE");
        } catch (Exception e) {
            System.out.println("Cleanup of schema " + schema + " failed: " + e.getMessage());
        }
    }

    private Flyway flywayFor(String schema, String location) {
        return Flyway.configure()
                .dataSource(jdbcUrl, "admin", null)
                .schemas(schema)
                .locations(location)
                .load();
    }

    /**
     * Reads {@code pg_index.indisvalid} for an index. DSQL marks an index INVALID while it is
     * still building, so this is what proves the migration waited.
     */
    private Boolean indexIsValid(String schema, String indexName) throws Exception {
        String sql = "SELECT indisvalid FROM pg_index"
                + " JOIN pg_class ON pg_class.oid = pg_index.indexrelid"
                + " JOIN pg_namespace ON pg_namespace.oid = pg_class.relnamespace"
                + " WHERE pg_class.relname = ? AND pg_namespace.nspname = ?";

        try (Connection conn = DriverManager.getConnection(jdbcUrl, "admin", null);
             PreparedStatement stmt = conn.prepareStatement(sql)) {
            stmt.setString(1, indexName);
            stmt.setString(2, schema);
            try (ResultSet rs = stmt.executeQuery()) {
                return rs.next() ? rs.getBoolean(1) : null;
            }
        }
    }

    @Test
    @Order(1)
    @DisplayName("Migration waits for a unique async index build to finish")
    void waitsForUniqueIndexBuild() throws Exception {
        String schema = createSchema();
        try {
            MigrateResult result = flywayFor(schema, "classpath:db/migration/asyncindex").migrate();

            assertTrue(result.success, "Migration should succeed");
            assertEquals(3, result.migrationsExecuted);

            // The moment migrate() returns, the index must already be active. Without the wait
            // this is false, because the build is still running in the background.
            Boolean valid = indexIsValid(schema, "idx_async_idx_users_email");
            assertNotNull(valid, "Index should exist after the migration");
            assertTrue(valid, "Index should already be valid when migrate() returns");
        } finally {
            dropSchema(schema);
        }
    }

    @Test
    @Order(2)
    @DisplayName("A failed unique index build fails the migration with the reason from sys.jobs")
    void failedIndexBuildFailsMigration() throws Exception {
        String schema = createSchema();
        try {
            Flyway flyway = flywayFor(schema, "classpath:db/migration/asyncindexfail");

            Exception thrown = assertThrows(Exception.class, flyway::migrate,
                    "A unique index build over duplicate data must fail the migration");

            String message = collectMessages(thrown);
            System.out.println("Failure reported to the user:\n" + message);
            assertTrue(message.contains("duplicate"),
                    "Failure should report the reason from sys.jobs, but was: " + message);
        } finally {
            dropSchema(schema);
        }
    }

    private String collectMessages(Throwable t) {
        StringBuilder sb = new StringBuilder();
        for (Throwable c = t; c != null && c != c.getCause(); c = c.getCause()) {
            sb.append(c.getMessage()).append('\n');
        }
        return sb.toString();
    }
}
