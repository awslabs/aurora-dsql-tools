/*
 * Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
 * SPDX-License-Identifier: Apache-2.0
 */
package software.amazon.dsql.flyway;

import org.flywaydb.core.extensibility.ConfigurationExtension;

/**
 * Configuration for Aurora DSQL behavior, under the {@code dsql} namespace.
 *
 * <p>OCC retry is opt-in: {@code occMaxRetries} defaults to {@code 0} (off). Set it to a positive
 * value to enable; {@code occMaxRetryDelaySeconds} caps the backoff (default 5s).
 *
 * <p>Async index waiting is opt-in, off by default. Set
 * {@code flyway.dsql.awaitAsyncIndexes=true} (or {@code FLYWAY_DSQL_AWAIT_ASYNC_INDEXES}) to
 * block on {@code CREATE INDEX ASYNC} builds until they complete.
 *
 * <p>Must remain a pure JavaBean: Flyway deep-copies it via {@link #copy()} using Jackson.
 * Do not store computed / non-bean state here.
 */
public class AuroraDSQLConfigurationExtension implements ConfigurationExtension {

    private static final String ENV_MAX_RETRIES = "FLYWAY_DSQL_OCC_MAX_RETRIES";
    private static final String ENV_MAX_RETRY_DELAY_SECONDS = "FLYWAY_DSQL_OCC_MAX_RETRY_DELAY_SECONDS";
    private static final String ENV_AWAIT_ASYNC_INDEXES = "FLYWAY_DSQL_AWAIT_ASYNC_INDEXES";

    private int occMaxRetries = 0;
    private int occMaxRetryDelaySeconds = 5;
    private boolean awaitAsyncIndexes = false;

    public int getOccMaxRetries() {
        return occMaxRetries;
    }

    public void setOccMaxRetries(int occMaxRetries) {
        this.occMaxRetries = occMaxRetries;
    }

    public int getOccMaxRetryDelaySeconds() {
        return occMaxRetryDelaySeconds;
    }

    public void setOccMaxRetryDelaySeconds(int occMaxRetryDelaySeconds) {
        this.occMaxRetryDelaySeconds = occMaxRetryDelaySeconds;
    }

    public boolean isAwaitAsyncIndexes() {
        return awaitAsyncIndexes;
    }

    public void setAwaitAsyncIndexes(boolean awaitAsyncIndexes) {
        this.awaitAsyncIndexes = awaitAsyncIndexes;
    }

    @Override
    public String getNamespace() {
        return "dsql";
    }

    @Override
    public String getConfigurationParameterFromEnvironmentVariable(String environmentVariable) {
        switch (environmentVariable) {
            case ENV_MAX_RETRIES:
                return "flyway.dsql.occMaxRetries";
            case ENV_MAX_RETRY_DELAY_SECONDS:
                return "flyway.dsql.occMaxRetryDelaySeconds";
            case ENV_AWAIT_ASYNC_INDEXES:
                return "flyway.dsql.awaitAsyncIndexes";
            default:
                return null;
        }
    }
}
