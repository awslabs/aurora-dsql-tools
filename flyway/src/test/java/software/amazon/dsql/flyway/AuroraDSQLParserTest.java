/*
 * Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
 * SPDX-License-Identifier: Apache-2.0
 */
package software.amazon.dsql.flyway;

import org.flywaydb.core.api.configuration.ClassicConfiguration;
import org.flywaydb.core.internal.jdbc.JdbcTemplate;
import org.flywaydb.core.internal.jdbc.Result;
import org.flywaydb.core.internal.jdbc.Results;
import org.flywaydb.core.internal.parser.ParsingContext;
import org.flywaydb.core.internal.resource.StringResource;
import org.flywaydb.core.internal.sqlscript.SqlStatement;
import org.flywaydb.core.internal.sqlscript.SqlStatementIterator;
import org.junit.jupiter.api.DisplayName;
import org.junit.jupiter.api.Test;

import java.util.ArrayList;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;
import java.util.logging.Handler;
import java.util.logging.Level;
import java.util.logging.LogRecord;
import java.util.logging.Logger;

import static org.junit.jupiter.api.Assertions.*;

/**
 * Unit tests for the DSQL parser's detection of asynchronous unique index creation.
 *
 * <p>No database connection required - these exercise the parser directly.</p>
 */
class AuroraDSQLParserTest {

    private List<SqlStatement> parse(String sql) {
        AuroraDSQLParser parser = new AuroraDSQLParser(new ClassicConfiguration(), new ParsingContext());
        List<SqlStatement> statements = new ArrayList<>();
        try (SqlStatementIterator iterator = parser.parse(new StringResource(sql))) {
            SqlStatement statement;
            while ((statement = iterator.next()) != null) {
                statements.add(statement);
            }
        }
        return statements;
    }

    private SqlStatement parseOne(String sql) {
        List<SqlStatement> statements = parse(sql);
        assertEquals(1, statements.size(), "Expected exactly one statement from: " + sql);
        return statements.get(0);
    }

    // ==================== statements that SHOULD be recognised ====================

    @Test
    @DisplayName("CREATE UNIQUE INDEX ASYNC is recognised and is non-transactional")
    void recognisesUniqueIndexAsync() {
        SqlStatement statement = parseOne("CREATE UNIQUE INDEX ASYNC idx_users_email ON users(email);");

        assertInstanceOf(AuroraDSQLAsyncIndexStatement.class, statement);
        assertFalse(statement.canExecuteInTransaction(),
                "The wait cannot run inside a transaction block, so the statement must be non-transactional");
    }

    @Test
    @DisplayName("Recognised regardless of keyword case and whitespace")
    void recognisesRegardlessOfCaseAndWhitespace() {
        assertInstanceOf(AuroraDSQLAsyncIndexStatement.class,
                parseOne("create unique index async idx ON t(c);"));
        assertInstanceOf(AuroraDSQLAsyncIndexStatement.class,
                parseOne("Create   Unique\n  Index\tAsync idx ON t(c);"));
    }

    @Test
    @DisplayName("Recognised with IF NOT EXISTS, INCLUDE and NULLS clauses")
    void recognisesWithTrailingClauses() {
        assertInstanceOf(AuroraDSQLAsyncIndexStatement.class,
                parseOne("CREATE UNIQUE INDEX ASYNC IF NOT EXISTS idx ON t(c NULLS LAST)"
                        + " INCLUDE (d) NULLS NOT DISTINCT;"));
    }

    @Test
    @DisplayName("Recognised when preceded by comments")
    void recognisesAfterLeadingComments() {
        SqlStatement statement = parseOne(
                "-- add a unique index\n"
                        + "/* still a comment */\n"
                        + "CREATE UNIQUE INDEX ASYNC idx ON t(c);");

        assertInstanceOf(AuroraDSQLAsyncIndexStatement.class, statement);
    }

    // ==================== statements that should NOT be recognised ====================

    @Test
    @DisplayName("Non-unique CREATE INDEX ASYNC is left alone")
    void ignoresNonUniqueAsyncIndex() {
        SqlStatement statement = parseOne("CREATE INDEX ASYNC idx_users_email ON users(email);");

        assertFalse(statement instanceof AuroraDSQLAsyncIndexStatement);
        assertTrue(statement.canExecuteInTransaction(),
                "A non-unique async index should keep PostgreSQL's default transactionality");
    }

    @Test
    @DisplayName("CREATE UNIQUE INDEX without ASYNC is left alone")
    void ignoresSynchronousUniqueIndex() {
        assertFalse(parseOne("CREATE UNIQUE INDEX idx ON t(c);")
                instanceof AuroraDSQLAsyncIndexStatement);
    }

    @Test
    @DisplayName("CREATE TABLE with a UNIQUE column is left alone")
    void ignoresCreateTableWithUniqueColumn() {
        assertFalse(parseOne("CREATE TABLE t (id INT PRIMARY KEY, name VARCHAR(100) NOT NULL UNIQUE);")
                instanceof AuroraDSQLAsyncIndexStatement);
    }

    @Test
    @DisplayName("DROP INDEX is left alone")
    void ignoresDropIndex() {
        assertFalse(parseOne("DROP INDEX idx;") instanceof AuroraDSQLAsyncIndexStatement);
    }

    @Test
    @DisplayName("A statement merely mentioning the keywords in a string is left alone")
    void ignoresKeywordsInsideStringLiteral() {
        assertFalse(parseOne("INSERT INTO log (msg) VALUES ('CREATE UNIQUE INDEX ASYNC idx ON t(c)');")
                instanceof AuroraDSQLAsyncIndexStatement);
    }

    // ==================== mixed scripts ====================

    @Test
    @DisplayName("Only the async unique index in a multi-statement script is recognised")
    void recognisesOnlyTheAsyncUniqueIndexInAScript() {
        List<SqlStatement> statements = parse(
                "CREATE TABLE t (id INT PRIMARY KEY, email VARCHAR(255));\n"
                        + "CREATE UNIQUE INDEX ASYNC idx ON t(email);\n"
                        + "CREATE INDEX ASYNC idx2 ON t(id);\n");

        assertEquals(3, statements.size());
        assertFalse(statements.get(0) instanceof AuroraDSQLAsyncIndexStatement);
        assertInstanceOf(AuroraDSQLAsyncIndexStatement.class, statements.get(1));
        assertFalse(statements.get(2) instanceof AuroraDSQLAsyncIndexStatement);
    }

    // ==================== job id extraction ====================

    @Test
    @DisplayName("firstValue reads a named column from a result set")
    void firstValueReadsNamedColumn() {
        Results results = new Results();
        results.addResult(new Result(-1, List.of("job_id"),
                List.of(List.of("ea7ykf5vkjhfzmbkriq7ywvc4a")), "CREATE UNIQUE INDEX ASYNC ..."));

        assertEquals("ea7ykf5vkjhfzmbkriq7ywvc4a",
                AuroraDSQLAsyncIndexStatement.firstValue(results, "job_id"));
        assertEquals("ea7ykf5vkjhfzmbkriq7ywvc4a",
                AuroraDSQLAsyncIndexStatement.firstValue(results, "JOB_ID"),
                "Column matching should be case-insensitive");
    }

    @Test
    @DisplayName("firstValue returns null rather than throwing on unexpected result shapes")
    void firstValueToleratesUnexpectedShapes() {
        Results empty = new Results();
        assertNull(AuroraDSQLAsyncIndexStatement.firstValue(empty, "job_id"));

        Results updateCountOnly = new Results();
        updateCountOnly.addResult(new Result(0, null, null, "sql"));
        assertNull(AuroraDSQLAsyncIndexStatement.firstValue(updateCountOnly, "job_id"));

        Results noRows = new Results();
        noRows.addResult(new Result(-1, List.of("job_id"), List.of(), "sql"));
        assertNull(AuroraDSQLAsyncIndexStatement.firstValue(noRows, "job_id"));

        Results otherColumn = new Results();
        otherColumn.addResult(new Result(-1, List.of("something_else"), List.of(List.of("x")), "sql"));
        assertNull(AuroraDSQLAsyncIndexStatement.firstValue(otherColumn, "job_id"));
    }

    @Test
    @DisplayName("firstValue finds the column when it is not in the first result")
    void firstValueScansAllResults() {
        Results results = new Results();
        results.addResult(new Result(0, null, null, "sql"));
        results.addResult(new Result(-1, List.of("a", "job_id"),
                List.of(List.of("x", "wlkqzeon6zhilipeeqy5hyseci")), "sql"));

        assertEquals("wlkqzeon6zhilipeeqy5hyseci",
                AuroraDSQLAsyncIndexStatement.firstValue(results, "job_id"));
    }

    // ==================== sys.jobs lookup ====================

    private static final String JOB_ID = "ea7ykf5vkjhfzmbkriq7ywvc4a";

    /** A JdbcTemplate that returns canned rows instead of talking to a database. */
    private static final class StubJdbcTemplate extends JdbcTemplate {
        private final List<Map<String, String>> rows;

        StubJdbcTemplate(List<Map<String, String>> rows) {
            // (Connection, DatabaseType) rather than (Connection, int): the latter does not exist
            // in Flyway 11, which this plugin supports. The connection is never used.
            super(null, new AuroraDSQLDatabaseType());
            this.rows = rows;
        }

        @Override
        public List<Map<String, String>> queryForList(String query, Object... params) {
            return rows;
        }
    }

    private static Map<String, String> jobRow(String status, String details, String objectName) {
        Map<String, String> row = new LinkedHashMap<>();
        row.put("status", status);
        row.put("details", details);
        row.put("object_name", objectName);
        return row;
    }

    private AuroraDSQLAsyncIndexStatement asyncIndexStatement() {
        return assertInstanceOf(AuroraDSQLAsyncIndexStatement.class,
                parseOne("CREATE UNIQUE INDEX ASYNC idx_users_email ON users(email);"));
    }

    /** Captures records published to the statement class's logger while {@code action} runs. */
    private List<LogRecord> captureLogs(Runnable action) {
        Logger logger = Logger.getLogger(AuroraDSQLAsyncIndexStatement.class.getName());
        List<LogRecord> records = new ArrayList<>();
        Handler handler = new Handler() {
            @Override
            public void publish(LogRecord record) {
                records.add(record);
            }

            @Override
            public void flush() {
            }

            @Override
            public void close() {
            }
        };

        logger.addHandler(handler);
        try {
            action.run();
        } finally {
            logger.removeHandler(handler);
        }
        return records;
    }

    @Test
    @DisplayName("queryJob warns when sys.jobs returns more than one row")
    void queryJobWarnsOnMultipleRows() {
        AuroraDSQLAsyncIndexStatement statement = asyncIndexStatement();
        StubJdbcTemplate jdbcTemplate = new StubJdbcTemplate(List.of(
                jobRow("processing", null, "public.idx_users_email"),
                jobRow("completed", null, "public.idx_something_else")));

        List<Map<String, String>> returned = new ArrayList<>();
        List<LogRecord> logs = captureLogs(() -> returned.add(statement.queryJob(jdbcTemplate, JOB_ID)));

        assertEquals("processing", returned.get(0).get("status"),
                "Should carry on with the first row");

        List<LogRecord> warnings = logs.stream()
                .filter(r -> r.getLevel() == Level.WARNING)
                .toList();
        assertEquals(1, warnings.size(), "Expected exactly one warning, got: " + logs);
        String message = warnings.get(0).getMessage();
        assertTrue(message.contains("2 rows"), "Warning should report the row count: " + message);
        assertTrue(message.contains(JOB_ID), "Warning should name the job: " + message);
    }

    @Test
    @DisplayName("queryJob does not warn for a single row")
    void queryJobDoesNotWarnOnSingleRow() {
        AuroraDSQLAsyncIndexStatement statement = asyncIndexStatement();
        StubJdbcTemplate jdbcTemplate = new StubJdbcTemplate(
                List.of(jobRow("completed", null, "public.idx_users_email")));

        List<Map<String, String>> returned = new ArrayList<>();
        List<LogRecord> logs = captureLogs(() -> returned.add(statement.queryJob(jdbcTemplate, JOB_ID)));

        assertEquals("completed", returned.get(0).get("status"));
        assertTrue(logs.stream().noneMatch(r -> r.getLevel() == Level.WARNING),
                "A single row is the normal case and must not warn: " + logs);
    }

    @Test
    @DisplayName("queryJob returns null without warning when sys.jobs has no matching row")
    void queryJobReturnsNullOnNoRows() {
        AuroraDSQLAsyncIndexStatement statement = asyncIndexStatement();
        StubJdbcTemplate jdbcTemplate = new StubJdbcTemplate(List.of());

        List<Map<String, String>> returned = new ArrayList<>();
        List<LogRecord> logs = captureLogs(() -> returned.add(statement.queryJob(jdbcTemplate, JOB_ID)));

        assertNull(returned.get(0), "An absent job is signalled by null");
        assertTrue(logs.stream().noneMatch(r -> r.getLevel() == Level.WARNING),
                "The absent-job case is reported by the caller, not here: " + logs);
    }
}
