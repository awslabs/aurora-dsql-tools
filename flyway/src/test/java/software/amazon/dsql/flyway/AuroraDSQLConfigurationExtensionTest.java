/*
 * Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
 * SPDX-License-Identifier: Apache-2.0
 */
package software.amazon.dsql.flyway;

import org.flywaydb.core.Flyway;
import org.flywaydb.core.api.configuration.ClassicConfiguration;
import org.flywaydb.core.api.configuration.FluentConfiguration;
import org.flywaydb.core.internal.parser.ParsingContext;
import org.flywaydb.core.internal.resource.StringResource;
import org.flywaydb.core.internal.sqlscript.SqlStatement;
import org.flywaydb.core.internal.sqlscript.SqlStatementIterator;
import org.junit.jupiter.api.DisplayName;
import org.junit.jupiter.api.Test;

import java.util.Properties;

import static org.junit.jupiter.api.Assertions.*;

/**
 * Tests for {@code flyway.dsql.waitForUniqueIndexBuilds}, which controls whether
 * {@code CREATE UNIQUE INDEX ASYNC} waits for the index build to finish.
 */
class AuroraDSQLConfigurationExtensionTest {

    private static final String UNIQUE_INDEX_ASYNC =
            "CREATE UNIQUE INDEX ASYNC idx_users_email ON users(email);";

    private SqlStatement parseOne(ClassicConfiguration configuration, String sql) {
        AuroraDSQLParser parser = new AuroraDSQLParser(configuration, new ParsingContext());
        try (SqlStatementIterator iterator = parser.parse(new StringResource(sql))) {
            SqlStatement statement = iterator.next();
            assertNotNull(statement, "Expected a statement from: " + sql);
            return statement;
        }
    }

    @SuppressWarnings("deprecation") // getPlugin(): matches the main source, for Flyway 11 support.
    private AuroraDSQLConfigurationExtension extensionOf(ClassicConfiguration configuration) {
        return configuration.getPluginRegister().getPlugin(AuroraDSQLConfigurationExtension.class);
    }

    @Test
    @DisplayName("The extension is discovered via the Plugin service loader")
    void extensionIsRegistered() {
        assertNotNull(extensionOf(new ClassicConfiguration()),
                "AuroraDSQLConfigurationExtension must be listed in"
                        + " META-INF/services/org.flywaydb.core.extensibility.Plugin,"
                        + " otherwise the setting can never be applied");
    }

    @Test
    @DisplayName("Waiting is enabled by default")
    void waitingIsOnByDefault() {
        ClassicConfiguration configuration = new ClassicConfiguration();

        assertTrue(extensionOf(configuration).shouldWaitForUniqueIndexBuilds());
        assertInstanceOf(AuroraDSQLAsyncIndexStatement.class, parseOne(configuration, UNIQUE_INDEX_ASYNC));
    }

    @Test
    @DisplayName("Setting waitForUniqueIndexBuilds=false restores plain parsing")
    void waitingCanBeDisabled() {
        ClassicConfiguration configuration = new ClassicConfiguration();
        extensionOf(configuration).setWaitForUniqueIndexBuilds(false);

        SqlStatement statement = parseOne(configuration, UNIQUE_INDEX_ASYNC);

        assertFalse(statement instanceof AuroraDSQLAsyncIndexStatement,
                "With waiting disabled the statement must not be the waiting variant");
        assertTrue(statement.canExecuteInTransaction(),
                "With waiting disabled the statement must go back to being transactional");
    }

    @Test
    @DisplayName("Setting waitForUniqueIndexBuilds=true explicitly keeps waiting on")
    void waitingCanBeEnabledExplicitly() {
        ClassicConfiguration configuration = new ClassicConfiguration();
        extensionOf(configuration).setWaitForUniqueIndexBuilds(true);

        assertInstanceOf(AuroraDSQLAsyncIndexStatement.class, parseOne(configuration, UNIQUE_INDEX_ASYNC));
    }

    @Test
    @DisplayName("The setting is read from flyway.dsql.waitForUniqueIndexBuilds")
    void settingIsReadFromProperties() {
        ClassicConfiguration configuration = new ClassicConfiguration();
        Properties properties = new Properties();
        properties.setProperty(AuroraDSQLConfigurationExtension.WAIT_FOR_UNIQUE_INDEX_BUILDS, "false");
        configuration.configure(properties);

        assertFalse(extensionOf(configuration).shouldWaitForUniqueIndexBuilds(),
                "The property name must bind to the extension field");
        assertFalse(parseOne(configuration, UNIQUE_INDEX_ASYNC) instanceof AuroraDSQLAsyncIndexStatement);
    }

    @Test
    @DisplayName("Unset is distinguishable from explicitly false, and resolves to enabled")
    void unsetResolvesToEnabled() {
        AuroraDSQLConfigurationExtension extension = new AuroraDSQLConfigurationExtension();

        assertNull(extension.getWaitForUniqueIndexBuilds(), "Unset must stay null, not default to a value");
        assertTrue(extension.shouldWaitForUniqueIndexBuilds(), "Unset resolves to enabled");

        extension.setWaitForUniqueIndexBuilds(false);
        assertEquals(Boolean.FALSE, extension.getWaitForUniqueIndexBuilds());
        assertFalse(extension.shouldWaitForUniqueIndexBuilds());

        extension.setWaitForUniqueIndexBuilds(null);
        assertTrue(extension.shouldWaitForUniqueIndexBuilds(), "Clearing it goes back to the default");
    }

    @Test
    @DisplayName("copy() round-trips the setting")
    void copyRoundTripsTheSetting() {
        // copy() serializes the extension, so a second accessor for the waitForUniqueIndexBuilds
        // bean property would break here rather than anywhere obvious.
        AuroraDSQLConfigurationExtension unset = new AuroraDSQLConfigurationExtension();
        assertEquals(unset, unset.copy());

        AuroraDSQLConfigurationExtension disabled = new AuroraDSQLConfigurationExtension();
        disabled.setWaitForUniqueIndexBuilds(false);

        AuroraDSQLConfigurationExtension copied =
                assertInstanceOf(AuroraDSQLConfigurationExtension.class, disabled.copy());
        assertEquals(Boolean.FALSE, copied.getWaitForUniqueIndexBuilds());
        assertFalse(copied.shouldWaitForUniqueIndexBuilds());
    }

    @Test
    @DisplayName("The environment variable maps to the configuration parameter")
    void environmentVariableIsMapped() {
        AuroraDSQLConfigurationExtension extension = new AuroraDSQLConfigurationExtension();

        assertEquals(AuroraDSQLConfigurationExtension.WAIT_FOR_UNIQUE_INDEX_BUILDS,
                extension.getConfigurationParameterFromEnvironmentVariable(
                        "FLYWAY_DSQL_WAIT_FOR_UNIQUE_INDEX_BUILDS"));
        assertNull(extension.getConfigurationParameterFromEnvironmentVariable("FLYWAY_SOMETHING_ELSE"));
    }

    @Test
    @DisplayName("The extension uses the dsql namespace")
    void namespaceIsDsql() {
        assertEquals("dsql", new AuroraDSQLConfigurationExtension().getNamespace());
    }

    @Test
    @DisplayName("The Java API shown in the README works")
    @SuppressWarnings("deprecation") // getPlugin(): matches the README, for Flyway 11 support.
    void documentedJavaApiWorks() {
        // Mirrors the snippet in README.md; kept as a test so the documentation cannot rot.
        FluentConfiguration configuration = Flyway.configure();
        configuration.getPluginRegister()
                .getPlugin(AuroraDSQLConfigurationExtension.class)
                .setWaitForUniqueIndexBuilds(false);

        assertFalse(configuration.getPluginRegister()
                .getPlugin(AuroraDSQLConfigurationExtension.class)
                .shouldWaitForUniqueIndexBuilds());
    }

    @Test
    @DisplayName("Disabling waiting does not affect other statements")
    void disablingDoesNotAffectOtherStatements() {
        ClassicConfiguration configuration = new ClassicConfiguration();
        extensionOf(configuration).setWaitForUniqueIndexBuilds(false);

        SqlStatement nonUnique = parseOne(configuration, "CREATE INDEX ASYNC idx ON t(c);");
        assertTrue(nonUnique.canExecuteInTransaction());

        SqlStatement createTable = parseOne(configuration, "CREATE TABLE t (id INT PRIMARY KEY);");
        assertTrue(createTable.canExecuteInTransaction());
    }
}
