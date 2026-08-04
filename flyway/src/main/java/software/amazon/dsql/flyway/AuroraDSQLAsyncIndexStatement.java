/*
 * Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
 * SPDX-License-Identifier: Apache-2.0
 */
package software.amazon.dsql.flyway;

import org.flywaydb.core.api.FlywayException;
import org.flywaydb.core.api.configuration.Configuration;
import org.flywaydb.core.internal.jdbc.JdbcTemplate;
import org.flywaydb.core.internal.jdbc.Result;
import org.flywaydb.core.internal.jdbc.Results;
import org.flywaydb.core.internal.sqlscript.Delimiter;
import org.flywaydb.core.internal.sqlscript.ParsedSqlStatement;
import org.flywaydb.core.internal.sqlscript.SqlScriptExecutor;

import java.sql.SQLException;
import java.util.List;
import java.util.Map;
import java.util.logging.Logger;
import java.util.regex.Pattern;

/**
 * A {@code CREATE UNIQUE INDEX ASYNC} statement that blocks until the index build finishes.
 *
 * <p>Aurora DSQL builds indexes asynchronously: the statement returns a {@code job_id}
 * immediately and the build continues in the background. When a <em>unique</em> index becomes
 * active, DSQL updates the system catalog, and sessions touching the same namespace at that
 * moment can fail with a concurrency error. Other sessions can retry, but the Flyway session
 * cannot - it would fail the migration.</p>
 *
 * <p>This statement therefore waits for its own build to finish before Flyway moves on, using
 * {@code sys.wait_for_job()}. Because DSQL rejects that call inside a transaction block, the
 * statement reports {@link #canExecuteInTransaction()} as {@code false}, which makes Flyway run
 * the enclosing script non-transactionally.</p>
 */
public class AuroraDSQLAsyncIndexStatement extends ParsedSqlStatement {

    private static final Logger LOG = Logger.getLogger(AuroraDSQLAsyncIndexStatement.class.getName());

    /** Column of the single-row result set returned by {@code CREATE ... INDEX ASYNC}. */
    private static final String JOB_ID_COLUMN = "job_id";

    /**
     * DSQL job ids are base32-encoded UUIDs: 26 characters of lowercase RFC 4648 base32.
     * Validated before use so the id can be inlined into the {@code CALL} safely.
     */
    private static final Pattern JOB_ID_REGEX = Pattern.compile("[a-z2-7]{26}");

    private static final String JOB_QUERY =
            "SELECT status, details, object_name FROM sys.jobs WHERE job_id = ?";

    private static final String STATUS_COMPLETED = "completed";
    private static final String STATUS_FAILED = "failed";

    /** Longest SQL fragment quoted back in an error message. */
    private static final int SQL_SUMMARY_LIMIT = 120;

    AuroraDSQLAsyncIndexStatement(int pos, int line, int col, String sql, Delimiter delimiter) {
        // canExecuteInTransaction=false: DSQL answers "CALL sys.wait_for_job not supported in
        // transaction block", so the wait - and therefore this statement - needs autocommit.
        // batchable=false: this statement's result set carries the job id we need.
        super(pos, line, col, sql, delimiter, false, false);
    }

    @Override
    public Results execute(JdbcTemplate jdbcTemplate, SqlScriptExecutor sqlScriptExecutor, Configuration config) {
        Results results = super.execute(jdbcTemplate, sqlScriptExecutor, config);

        if (results.getException() != null) {
            // The CREATE INDEX itself failed. Let Flyway report it as it would any other statement.
            return results;
        }

        String jobId = firstValue(results, JOB_ID_COLUMN);
        if (jobId == null || !JOB_ID_REGEX.matcher(jobId).matches()) {
            // The index was created, so failing here would record a failed migration for a
            // statement that actually succeeded - and DSQL cannot roll the DDL back. Warn instead.
            LOG.warning("Aurora DSQL returned no usable " + JOB_ID_COLUMN + " for [" + sqlSummary()
                    + (jobId == null ? "]" : "] (got \"" + jobId + "\")")
                    + "; not waiting for the index build. A concurrent index activation may cause"
                    + " the next statement in this migration to fail with a concurrency error.");
            return results;
        }

        awaitIndexBuild(jdbcTemplate, jobId);
        return results;
    }

    /**
     * Blocks until the given index build job reaches a terminal state, failing the migration if
     * the build did not succeed.
     */
    private void awaitIndexBuild(JdbcTemplate jdbcTemplate, String jobId) {
        // sys.wait_for_job() blocks forever on a job id it cannot find: there is no existence
        // check, no error and no timeout, and DSQL refuses to let statement_timeout be set. So
        // confirm the job exists before ever waiting on it.
        Map<String, String> job = queryJob(jdbcTemplate, jobId);
        if (job == null) {
            throw new FlywayException("Aurora DSQL reported index build job " + jobId + " for ["
                    + sqlSummary() + "] but no such job is present in sys.jobs. Refusing to wait,"
                    + " because sys.wait_for_job() blocks indefinitely on an unknown job id.");
        }

        String index = indexName(job);
        String status = job.get("status");
        if (STATUS_COMPLETED.equalsIgnoreCase(status)) {
            LOG.fine("Aurora DSQL index build " + index + " (job " + jobId + ") had already completed");
            return;
        }
        if (STATUS_FAILED.equalsIgnoreCase(status)) {
            throw buildFailed(index, jobId, job.get("details"));
        }

        LOG.info("Waiting for Aurora DSQL to finish building index " + index
                + " (job " + jobId + ", status: " + status + ") ...");
        // The job id is inlined rather than bound because this is a CALL that returns a result
        // set; it is safe because it has been matched against JOB_ID_REGEX above.
        Results waitResults = jdbcTemplate.executeStatement("CALL sys.wait_for_job('" + jobId + "')");
        if (waitResults.getException() != null) {
            throw new FlywayException("Failed to wait for Aurora DSQL index build " + index
                    + " (job " + jobId + ")", waitResults.getException());
        }

        // sys.wait_for_job() reports its verdict through an INOUT boolean, which reaches us as
        // driver-dependent text ("t" or "true"). Re-read sys.jobs instead: it is unambiguous and
        // carries the failure reason we would need anyway.
        job = queryJob(jdbcTemplate, jobId);
        status = job == null ? null : job.get("status");

        if (STATUS_COMPLETED.equalsIgnoreCase(status)) {
            LOG.info("Aurora DSQL finished building index " + index);
            return;
        }
        if (STATUS_FAILED.equalsIgnoreCase(status)) {
            throw buildFailed(index, jobId, job.get("details"));
        }
        throw new FlywayException("sys.wait_for_job() returned for Aurora DSQL index build "
                + index + " (job " + jobId + "), but the job is now reported as "
                + (status == null ? "absent from sys.jobs" : "\"" + status + "\"")
                + " rather than completed or failed.");
    }

    private FlywayException buildFailed(String index, String jobId, String details) {
        return new FlywayException("Aurora DSQL failed to build index " + index
                + " (job " + jobId + "): "
                + (details == null || details.isBlank() ? "no reason reported in sys.jobs" : details)
                + ". DSQL leaves a failed index in place but INVALID; drop it and recreate it once"
                + " the cause is resolved.");
    }

    /**
     * Names the index being built, preferring the name DSQL itself reports over the statement text.
     */
    private String indexName(Map<String, String> job) {
        String objectName = job.get("object_name");
        return objectName == null || objectName.isBlank() ? "for [" + sqlSummary() + "]" : objectName;
    }

    /**
     * A single-line, length-capped rendering of this statement, for error messages. The raw SQL
     * can carry leading comments and newlines from the migration file.
     */
    private String sqlSummary() {
        String summary = getSql().replaceAll("\\s+", " ").trim();
        return summary.length() <= SQL_SUMMARY_LIMIT
                ? summary
                : summary.substring(0, SQL_SUMMARY_LIMIT - 3) + "...";
    }

    /**
     * Returns the {@code sys.jobs} row for this job, or {@code null} if there is no such row.
     *
     * <p>Package-private so it can be exercised directly by unit tests.</p>
     */
    Map<String, String> queryJob(JdbcTemplate jdbcTemplate, String jobId) {
        try {
            List<Map<String, String>> rows = jdbcTemplate.queryForList(JOB_QUERY, jobId);
            if (rows.isEmpty()) {
                return null;
            }
            if (rows.size() > 1) {
                // job_id is a job's identity, so this should not be possible. Warn but continue on.
                LOG.warning("sys.jobs returned " + rows.size() + " rows for Aurora DSQL index build"
                        + " job " + jobId + ", but a job id identifies at most one job."
                        + " Continuing with the first row; the reported build status may not be"
                        + " the one for [" + sqlSummary() + "].");
            }
            return rows.get(0);
        } catch (SQLException e) {
            throw new FlywayException("Unable to query sys.jobs for Aurora DSQL index build job "
                    + jobId + " for [" + getSql() + "]", e);
        }
    }

    /**
     * Reads the first row's value of the named column from a statement's results.
     *
     * @return the value, or {@code null} if no result set carried a column of that name.
     */
    static String firstValue(Results results, String column) {
        for (Result result : results.getResults()) {
            List<String> columns = result.columns();
            List<List<String>> data = result.data();
            if (columns == null || data == null || data.isEmpty()) {
                continue;
            }
            for (int i = 0; i < columns.size(); i++) {
                if (column.equalsIgnoreCase(columns.get(i))) {
                    List<String> row = data.get(0);
                    return i < row.size() ? row.get(i) : null;
                }
            }
        }
        return null;
    }
}
