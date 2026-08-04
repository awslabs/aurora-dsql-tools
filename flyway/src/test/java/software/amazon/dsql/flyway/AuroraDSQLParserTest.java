/*
 * Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
 * SPDX-License-Identifier: Apache-2.0
 */
package software.amazon.dsql.flyway;

import org.flywaydb.core.api.configuration.Configuration;
import org.flywaydb.core.api.configuration.FluentConfiguration;
import org.flywaydb.core.internal.parser.Parser;
import org.flywaydb.core.internal.parser.ParsingContext;
import org.flywaydb.database.postgresql.PostgreSQLParser;
import org.junit.jupiter.api.Test;

import static org.assertj.core.api.Assertions.assertThat;

class AuroraDSQLParserTest {

    @Test
    void isAPostgresParser() {
        Configuration config = new FluentConfiguration();
        Parser parser = new AuroraDSQLParser(config, new ParsingContext());
        assertThat(parser).isInstanceOf(PostgreSQLParser.class);
    }
}
