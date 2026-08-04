/*
 * Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
 * SPDX-License-Identifier: Apache-2.0
 */
package software.amazon.dsql.flyway;

import org.flywaydb.core.extensibility.ConfigurationExtension;
import org.junit.jupiter.api.Test;

import static org.assertj.core.api.Assertions.assertThat;

class AuroraDSQLConfigurationExtensionTest {

    private final AuroraDSQLConfigurationExtension ext = new AuroraDSQLConfigurationExtension();

    @Test
    void namespaceIsDsql() {
        assertThat(ext.getNamespace()).isEqualTo("dsql");
    }

    @Test
    void occRetryIsOffByDefault() {
        assertThat(ext.getOccMaxRetries()).isZero();
        assertThat(ext.getOccMaxRetryDelaySeconds()).isEqualTo(5);
    }

    @Test
    void awaitAsyncIndexesDefaultsToFalse() {
        assertThat(ext.isAwaitAsyncIndexes()).isFalse();
    }

    @Test
    void settersRoundTrip() {
        ext.setOccMaxRetries(2);
        ext.setOccMaxRetryDelaySeconds(10);
        ext.setAwaitAsyncIndexes(true);
        assertThat(ext.getOccMaxRetries()).isEqualTo(2);
        assertThat(ext.getOccMaxRetryDelaySeconds()).isEqualTo(10);
        assertThat(ext.isAwaitAsyncIndexes()).isTrue();
    }

    @Test
    void envVarsMapToFlywayPrefixedKeys() {
        assertThat(ext.getConfigurationParameterFromEnvironmentVariable("FLYWAY_DSQL_OCC_MAX_RETRIES"))
                .isEqualTo("flyway.dsql.occMaxRetries");
        assertThat(ext.getConfigurationParameterFromEnvironmentVariable("FLYWAY_DSQL_OCC_MAX_RETRY_DELAY_SECONDS"))
                .isEqualTo("flyway.dsql.occMaxRetryDelaySeconds");
        assertThat(ext.getConfigurationParameterFromEnvironmentVariable("FLYWAY_DSQL_AWAIT_ASYNC_INDEXES"))
                .isEqualTo("flyway.dsql.awaitAsyncIndexes");
        assertThat(ext.getConfigurationParameterFromEnvironmentVariable("FLYWAY_DSQL_UNKNOWN")).isNull();
    }

    // flyway-core (ConfigUtils) returns this value verbatim as the resolved property key, so it
    // must be fully qualified with the "flyway." prefix — matching every core extension and the
    // ClickHouse sibling ("flyway.<namespace>.<property>"). Without the prefix the env override
    // never resolves (the bug this guards against).
    @Test
    void envKeysAreFullyQualifiedWithFlywayAndNamespace() {
        String prefix = "flyway." + ext.getNamespace() + ".";
        assertThat(ext.getConfigurationParameterFromEnvironmentVariable("FLYWAY_DSQL_OCC_MAX_RETRIES"))
                .startsWith(prefix);
        assertThat(ext.getConfigurationParameterFromEnvironmentVariable("FLYWAY_DSQL_OCC_MAX_RETRY_DELAY_SECONDS"))
                .startsWith(prefix);
        assertThat(ext.getConfigurationParameterFromEnvironmentVariable("FLYWAY_DSQL_AWAIT_ASYNC_INDEXES"))
                .startsWith(prefix);
    }

    @Test
    void copyPreservesValuesAndDoesNotThrow() {
        ext.setOccMaxRetries(4);
        ext.setOccMaxRetryDelaySeconds(15);
        ext.setAwaitAsyncIndexes(true);
        ConfigurationExtension copy = (ConfigurationExtension) ext.copy();
        assertThat(copy).isInstanceOf(AuroraDSQLConfigurationExtension.class);
        AuroraDSQLConfigurationExtension c = (AuroraDSQLConfigurationExtension) copy;
        assertThat(c.getOccMaxRetries()).isEqualTo(4);
        assertThat(c.getOccMaxRetryDelaySeconds()).isEqualTo(15);
        assertThat(c.isAwaitAsyncIndexes()).isTrue();
    }
}
