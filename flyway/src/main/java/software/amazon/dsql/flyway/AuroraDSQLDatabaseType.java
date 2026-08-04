/*
 * Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
 * SPDX-License-Identifier: Apache-2.0
 */
package software.amazon.dsql.flyway;

import org.flywaydb.core.api.ResourceProvider;
import org.flywaydb.core.api.configuration.Configuration;
import org.flywaydb.core.internal.callback.CallbackExecutor;
import org.flywaydb.core.internal.database.base.Database;
import org.flywaydb.core.internal.jdbc.ExecutionTemplate;
import org.flywaydb.core.internal.jdbc.JdbcConnectionFactory;
import org.flywaydb.core.internal.jdbc.StatementInterceptor;
import org.flywaydb.core.internal.parser.Parser;
import org.flywaydb.core.internal.parser.ParsingContext;
import org.flywaydb.core.internal.sqlscript.SqlScriptExecutorFactory;
import org.flywaydb.database.postgresql.PostgreSQLDatabaseType;
import software.amazon.dsql.jdbc.OCCRetryConfig;

import java.net.URI;
import java.net.URISyntaxException;
import java.sql.Connection;
import java.sql.SQLException;

/**
 * Flyway database type for Amazon Aurora DSQL.
 *
 * <p>Extends PostgreSQL support to handle {@code jdbc:aws-dsql:} URLs and DSQL endpoints.
 * The Aurora DSQL JDBC connector transforms {@code jdbc:aws-dsql:postgresql://} URLs to
 * {@code jdbc:postgresql://} internally, so DSQL is detected by both the URL prefix and the
 * endpoint host pattern ({@code .dsql.} for public endpoints, {@code .dsql-} for PrivateLink).
 */
public class AuroraDSQLDatabaseType extends PostgreSQLDatabaseType {

    private static final String DSQL_PUBLIC_PATTERN = ".dsql.";
    private static final String DSQL_PRIVATELINK_PATTERN = ".dsql-";
    private static final int DEFAULT_MAX_RETRIES = 0;
    private static final int DEFAULT_MAX_RETRY_DELAY_SECONDS = 5;
    private static final long MIN_RETRY_DELAY_MS = 100L;

    // DatabaseType is a JVM-global singleton resolved from a static registry, and the commit-wrapping
    // seam (createTransactionalExecutionTemplate) receives no Configuration. Flyway runs a migration
    // synchronously on one thread, so we stash the per-run OCC retry knobs (resolved in createDatabase)
    // in a ThreadLocal and read them back when building the transactional template on the same thread.
    private static final ThreadLocal<int[]> OCC_RETRY_KNOBS =
            ThreadLocal.withInitial(() -> new int[]{DEFAULT_MAX_RETRIES, DEFAULT_MAX_RETRY_DELAY_SECONDS});

    @Override
    public String getName() {
        return "Aurora DSQL";
    }

    @Override
    public boolean handlesJDBCUrl(String url) {
        if (url.startsWith("jdbc:aws-dsql:")) {
            return true;
        }
        return url.startsWith("jdbc:postgresql://") && isDsqlHost(url);
    }

    /**
     * Matches the DSQL endpoint pattern against the URL's host only, so a {@code .dsql.} appearing
     * elsewhere (database name, query parameter) cannot misclassify a plain PostgreSQL URL as DSQL.
     */
    private static boolean isDsqlHost(String jdbcUrl) {
        String host = extractHost(jdbcUrl);
        return host != null
                && (host.contains(DSQL_PUBLIC_PATTERN) || host.contains(DSQL_PRIVATELINK_PATTERN));
    }

    /** Extracts the host from a {@code jdbc:...} URL, or null if it cannot be parsed. */
    private static String extractHost(String jdbcUrl) {
        int scheme = jdbcUrl.indexOf("://");
        if (scheme < 0) {
            return null;
        }
        try {
            // Strip the "jdbc:" prefix so java.net.URI sees a single, parseable scheme.
            URI uri = new URI(jdbcUrl.substring(jdbcUrl.indexOf(':') + 1));
            return uri.getHost();
        } catch (URISyntaxException e) {
            return null;
        }
    }

    @Override
    public int getPriority() {
        // Higher than PostgreSQL (0) so DSQL URLs are matched first.
        return 1;
    }

    @Override
    public boolean handlesDatabaseProductNameAndVersion(String databaseProductName,
                                                        String databaseProductVersion,
                                                        Connection connection) {
        try {
            String url = connection.getMetaData().getURL();
            return url != null && isDsqlHost(url);
        } catch (SQLException e) {
            return false;
        }
    }

    @Override
    public String getDriverClass(String url, ClassLoader classLoader) {
        if (url.startsWith("jdbc:aws-dsql:")) {
            return "software.amazon.dsql.jdbc.DSQLConnector";
        }
        return "org.postgresql.Driver";
    }

    @Override
    public Database createDatabase(Configuration configuration,
                                   JdbcConnectionFactory jdbcConnectionFactory,
                                   StatementInterceptor statementInterceptor) {
        OCC_RETRY_KNOBS.set(resolveOccRetryKnobs(configuration));
        return new AuroraDSQLDatabase(configuration, jdbcConnectionFactory, statementInterceptor);
    }

    /**
     * Wraps the base transactional template so a commit-time OCC conflict retries the whole
     * migration transaction. This is the only Flyway seam that spans {@code connection.commit()},
     * where DSQL surfaces {@code OC000}/{@code OC001}/{@code 40001}. Retry is opt-in: with the
     * default {@code occMaxRetries=0} the wrapper is a no-op that rethrows on the first conflict.
     */
    @Override
    public ExecutionTemplate createTransactionalExecutionTemplate(Connection connection, boolean rollbackOnException) {
        ExecutionTemplate delegate = super.createTransactionalExecutionTemplate(connection, rollbackOnException);
        int[] knobs = OCC_RETRY_KNOBS.get();
        // maxRetries/maxDelay come from the configured knobs (maxRetries defaults to 0 = off);
        // baseDelay/multiplier/jitter keep the connector defaults so the contract stays shared.
        OCCRetryConfig retryConfig = OCCRetryConfig.builder()
                .maxRetries(knobs[0])
                .maxDelayMs(maxRetryDelayMillis(knobs[1]))
                .build();
        return new AuroraDSQLExecutionTemplate(delegate, retryConfig);
    }

    /** Converts the configured delay cap (seconds) to milliseconds, floored at {@link #MIN_RETRY_DELAY_MS}. */
    static long maxRetryDelayMillis(int maxRetryDelaySeconds) {
        return Math.max(MIN_RETRY_DELAY_MS, (long) maxRetryDelaySeconds * 1000L);
    }

    static int[] resolveOccRetryKnobs(Configuration configuration) {
        if (configuration == null || configuration.getPluginRegister() == null) {
            return new int[]{DEFAULT_MAX_RETRIES, DEFAULT_MAX_RETRY_DELAY_SECONDS};
        }
        AuroraDSQLConfigurationExtension ext =
                configuration.getPluginRegister().getPlugin(AuroraDSQLConfigurationExtension.class);
        if (ext == null) {
            return new int[]{DEFAULT_MAX_RETRIES, DEFAULT_MAX_RETRY_DELAY_SECONDS};
        }
        // OCCRetryConfig.build() rejects maxRetries outside [0, 100]; clamp so a stray config value
        // degrades gracefully instead of failing the migration.
        int maxRetries = Math.min(100, Math.max(0, ext.getOccMaxRetries()));
        return new int[]{maxRetries, ext.getOccMaxRetryDelaySeconds()};
    }

    /**
     * Wraps the base SQL-script executor factory so that, when {@code flyway.dsql.awaitAsyncIndexes}
     * is enabled, a migration's {@code CREATE INDEX ASYNC} blocks until the index build completes.
     * DSQL builds indexes in the background and returns a runtime {@code job_id}; static SQL cannot
     * thread that id into {@code sys.wait_for_job}, so the wait is issued here. Off by default. See
     * {@link AuroraDSQLSqlScriptExecutor}.
     */
    @Override
    public SqlScriptExecutorFactory createSqlScriptExecutorFactory(
            JdbcConnectionFactory jdbcConnectionFactory,
            CallbackExecutor callbackExecutor,
            StatementInterceptor statementInterceptor) {
        SqlScriptExecutorFactory delegate = super.createSqlScriptExecutorFactory(
                jdbcConnectionFactory, callbackExecutor, statementInterceptor);
        return new AuroraDSQLSqlScriptExecutorFactory(delegate);
    }

    @Override
    public Parser createParser(Configuration configuration, ResourceProvider resourceProvider,
                               ParsingContext parsingContext) {
        return new AuroraDSQLParser(configuration, parsingContext);
    }

    public String getPluginVersion(Configuration config) {
        return AuroraDSQLDatabase.PLUGIN_VERSION;
    }
}
