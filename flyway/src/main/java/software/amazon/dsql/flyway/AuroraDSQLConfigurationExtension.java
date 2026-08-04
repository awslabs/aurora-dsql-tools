/*
 * Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
 * SPDX-License-Identifier: Apache-2.0
 */
package software.amazon.dsql.flyway;

import org.flywaydb.core.extensibility.ConfigurationExtension;

import java.util.Objects;

/**
 * Aurora DSQL specific Flyway configuration.
 *
 * <p>Settings live under the {@code dsql} namespace, so they are given as
 * {@code flyway.dsql.<name>} in configuration files, or as {@code FLYWAY_DSQL_<NAME>} in the
 * environment.</p>
 */
public class AuroraDSQLConfigurationExtension implements ConfigurationExtension {

    static final String WAIT_FOR_UNIQUE_INDEX_BUILDS = "flyway.dsql.waitForUniqueIndexBuilds";

    private static final String WAIT_FOR_UNIQUE_INDEX_BUILDS_ENV = "FLYWAY_DSQL_WAIT_FOR_UNIQUE_INDEX_BUILDS";

    /**
     * Whether {@code CREATE UNIQUE INDEX ASYNC} should block until DSQL has finished building the
     * index. {@code null} means unset, which is treated as enabled - see
     * {@link #shouldWaitForUniqueIndexBuilds()}.
     *
     * <p>When enabled, the plugin captures the {@code job_id} returned by the statement and waits
     * on it with {@code sys.wait_for_job()}, so that the activation of the new index cannot make a
     * later statement in the migration fail with a concurrency error. The cost is that the
     * statement takes as long as the index build, and that it has to run outside a transaction.</p>
     *
     * <p>Setting this to {@code false} restores plain Flyway behaviour: the statement returns as
     * soon as the build has been submitted, and stays transactional.</p>
     *
     * <p>Nullable rather than a primitive to match Flyway's own boolean settings, which keep
     * "unset" distinguishable from "explicitly false" so that defaults and overrides can be
     * layered. The field name is significant: configuration keys are bound to declared field
     * names.</p>
     */
    private Boolean waitForUniqueIndexBuilds;

    public Boolean getWaitForUniqueIndexBuilds() {
        return waitForUniqueIndexBuilds;
    }

    public void setWaitForUniqueIndexBuilds(Boolean waitForUniqueIndexBuilds) {
        this.waitForUniqueIndexBuilds = waitForUniqueIndexBuilds;
    }

    /**
     * Resolves {@link #waitForUniqueIndexBuilds} against its default, unset meaning enabled.
     *
     * <p>Deliberately not named {@code isWaitForUniqueIndexBuilds()}: that would make a second
     * accessor for the same bean property as {@link #getWaitForUniqueIndexBuilds()}, which the
     * serialization behind {@link #copy()} would reject. Flyway core has the same split, with the
     * nullable field on the configuration model and the resolution on {@code ClassicConfiguration}
     * (compare {@code isPlaceholderReplacement()}).</p>
     *
     * @return {@code true} unless waiting has been explicitly turned off.
     */
    public boolean shouldWaitForUniqueIndexBuilds() {
        return waitForUniqueIndexBuilds == null || waitForUniqueIndexBuilds;
    }

    @Override
    public String getConfigurationParameterFromEnvironmentVariable(String environmentVariable) {
        if (WAIT_FOR_UNIQUE_INDEX_BUILDS_ENV.equals(environmentVariable)) {
            return WAIT_FOR_UNIQUE_INDEX_BUILDS;
        }
        return null;
    }

    @Override
    public String getNamespace() {
        return "dsql";
    }

    @Override
    public boolean equals(Object o) {
        if (this == o) {
            return true;
        }
        if (!(o instanceof AuroraDSQLConfigurationExtension)) {
            return false;
        }
        return Objects.equals(waitForUniqueIndexBuilds,
                ((AuroraDSQLConfigurationExtension) o).waitForUniqueIndexBuilds);
    }

    @Override
    public int hashCode() {
        return Objects.hash(waitForUniqueIndexBuilds);
    }

    @Override
    public String toString() {
        return "AuroraDSQLConfigurationExtension(waitForUniqueIndexBuilds=" + waitForUniqueIndexBuilds + ")";
    }
}
