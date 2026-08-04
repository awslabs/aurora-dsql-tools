/*
 * Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
 * SPDX-License-Identifier: Apache-2.0
 */
package software.amazon.dsql.flyway;

import org.flywaydb.core.api.configuration.Configuration;
import org.flywaydb.core.internal.parser.ParserContext;
import org.flywaydb.core.internal.parser.ParsingContext;
import org.flywaydb.core.internal.parser.PeekingReader;
import org.flywaydb.core.internal.parser.Recorder;
import org.flywaydb.core.internal.parser.StatementType;
import org.flywaydb.core.internal.parser.Token;
import org.flywaydb.core.internal.sqlscript.Delimiter;
import org.flywaydb.core.internal.sqlscript.ParsedSqlStatement;
import org.flywaydb.database.postgresql.PostgreSQLParser;

import java.io.IOException;
import java.util.List;
import java.util.regex.Pattern;

/**
 * Aurora DSQL SQL parser.
 *
 * <p>Extends the PostgreSQL parser to recognise {@code CREATE UNIQUE INDEX ASYNC} and hand it to
 * {@link AuroraDSQLAsyncIndexStatement}, which waits for the asynchronous build to finish. This
 * mirrors how the PostgreSQL parser recognises {@code COPY ... FROM STDIN} and
 * {@code CREATE INDEX CONCURRENTLY}.</p>
 *
 * <p>Only <em>unique</em> async indexes are treated specially. It is the activation of a unique
 * index that makes concurrent sessions fail, and waiting is not free - it turns a fast DDL
 * statement into one that blocks for the length of the index build.</p>
 *
 * <p>The behaviour is on by default and can be turned off with
 * {@code flyway.dsql.waitForUniqueIndexBuilds=false}, in which case these statements are parsed
 * exactly as PostgreSQL would parse them. See {@link AuroraDSQLConfigurationExtension}.</p>
 */
public class AuroraDSQLParser extends PostgreSQLParser {

    /**
     * Matched against the parser's simplified statement, which accumulates one uppercased keyword
     * at a time - so this matches exactly when the fourth keyword has been read.
     */
    private static final Pattern UNIQUE_INDEX_ASYNC_REGEX = Pattern.compile("^CREATE UNIQUE INDEX ASYNC");

    private static final StatementType UNIQUE_INDEX_ASYNC = new StatementType();

    /**
     * Whether to treat async unique index creation specially, from
     * {@code flyway.dsql.waitForUniqueIndexBuilds}. Resolved once: a parser is created per script,
     * and the two detection hooks below are called for every keyword of every statement.
     */
    private final boolean waitForUniqueIndexBuilds;

    public AuroraDSQLParser(Configuration configuration, ParsingContext parsingContext) {
        super(configuration, parsingContext);
        this.waitForUniqueIndexBuilds = waitForUniqueIndexBuilds(configuration);
    }

    @SuppressWarnings("deprecation") // getPlugin(): required for Flyway 11 compatibility
    private static boolean waitForUniqueIndexBuilds(Configuration configuration) {
        AuroraDSQLConfigurationExtension extension = configuration.getPluginRegister()
                .getPlugin(AuroraDSQLConfigurationExtension.class);
        // Absent extension means nothing could have configured it, so fall back to the default.
        return extension == null || extension.shouldWaitForUniqueIndexBuilds();
    }

    /**
     * Whether this statement is an async unique index creation that we should wait on.
     */
    private boolean isAwaitedUniqueIndexAsync(String simplifiedStatement) {
        return waitForUniqueIndexBuilds && UNIQUE_INDEX_ASYNC_REGEX.matcher(simplifiedStatement).matches();
    }

    @Override
    protected StatementType detectStatementType(String simplifiedStatement, ParserContext context,
                                                PeekingReader reader) {
        if (isAwaitedUniqueIndexAsync(simplifiedStatement)) {
            return UNIQUE_INDEX_ASYNC;
        }
        return super.detectStatementType(simplifiedStatement, context, reader);
    }

    @Override
    protected Boolean detectCanExecuteInTransaction(String simplifiedStatement, List<Token> keywords) {
        if (isAwaitedUniqueIndexAsync(simplifiedStatement)) {
            // DSQL rejects "CALL sys.wait_for_job" inside a transaction block, so the statement
            // and its wait have to run with autocommit on.
            return false;
        }
        return super.detectCanExecuteInTransaction(simplifiedStatement, keywords);
    }

    @Override
    protected ParsedSqlStatement createStatement(PeekingReader reader, Recorder recorder,
                                                 int statementPos, int statementLine, int statementCol,
                                                 int nonCommentPartPos, int nonCommentPartLine,
                                                 int nonCommentPartCol, StatementType statementType,
                                                 boolean canExecuteInTransaction, Delimiter delimiter,
                                                 String sql, List<Token> tokens, boolean batchable)
            throws IOException {

        if (statementType == UNIQUE_INDEX_ASYNC) {
            return new AuroraDSQLAsyncIndexStatement(statementPos, statementLine, statementCol, sql, delimiter);
        }

        return super.createStatement(reader, recorder, statementPos, statementLine, statementCol,
                nonCommentPartPos, nonCommentPartLine, nonCommentPartCol, statementType,
                canExecuteInTransaction, delimiter, sql, tokens, batchable);
    }
}
