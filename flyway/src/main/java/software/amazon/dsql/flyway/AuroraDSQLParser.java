/*
 * Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
 * SPDX-License-Identifier: Apache-2.0
 */
package software.amazon.dsql.flyway;

import org.flywaydb.core.api.configuration.Configuration;
import org.flywaydb.core.internal.parser.ParsingContext;
import org.flywaydb.database.postgresql.PostgreSQLParser;

public class AuroraDSQLParser extends PostgreSQLParser {
    public AuroraDSQLParser(Configuration configuration, ParsingContext parsingContext) {
        super(configuration, parsingContext);
    }
}
