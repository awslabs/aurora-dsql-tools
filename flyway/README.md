# flyway-database-dsql

Flyway community database support for [Amazon Aurora DSQL](https://docs.aws.amazon.com/aurora-dsql/).

This directory packages the official
[`flyway-database-dsql` 10.26.0 source](https://github.com/flyway/flyway-community-db-support/tree/10.26.0/flyway-database-dsql)
at upstream commit `64985070bc0cc233b481b99088a4af53071f3d4b` under temporary AWS Maven
coordinates. The implementation and tests are unchanged; only the standalone build and publication
metadata differ.

## Installation

Version `3.0.0` requires Java 17 or later and uses the same Flyway `11.9.0` baseline as the official
`10.26.0` module.

```xml
<dependency>
    <groupId>software.amazon.dsql</groupId>
    <artifactId>aurora-dsql-flyway-support</artifactId>
    <version>3.0.0</version>
</dependency>
<dependency>
    <groupId>org.flywaydb</groupId>
    <artifactId>flyway-core</artifactId>
    <version>11.9.0</version>
</dependency>
<dependency>
    <groupId>software.amazon.dsql</groupId>
    <artifactId>aurora-dsql-jdbc-connector</artifactId>
    <version>1.5.0</version>
</dependency>
```

The support artifact brings in `flyway-database-postgresql:11.9.0`. Do not load this artifact and
`org.flywaydb:flyway-database-dsql` in the same Flyway runtime.

### Moving to the official artifact

When Redgate publishes the official module to your approved repository, remove
`software.amazon.dsql:aurora-dsql-flyway-support` and add
`org.flywaydb:flyway-database-dsql` at the corresponding official version. No Java package,
Flyway configuration, or migration changes are required because this artifact uses the official
implementation unchanged.

## DSQL-Specific Behavior

This module adapts Flyway's PostgreSQL support for Aurora DSQL's distributed architecture:

- **One DDL per transaction**: DSQL accepts a single DDL statement per transaction, with DML in its
  own. Flyway commits a migration file as one transaction, so keep one DDL statement per file (see
  [One DDL statement per migration](#one-ddl-statement-per-migration))
- **IAM authentication**: role-based access via IAM replaces PostgreSQL's `SET ROLE`
- **Optimistic concurrency**: DSQL uses OCC instead of advisory locks, and surfaces conflicts
  (`OC000` / `OC001` / `40001`) at commit time. The module can retry the migration transaction on a
  conflict — off by default, opt in via [Configuration](#configuration). Run migrations from a single instance
- **Async indexes**: use `CREATE INDEX ASYNC` in migrations, with an optional wait for the build
  (see [Asynchronous indexes](#asynchronous-indexes))

## Requirements

- The [Aurora DSQL JDBC connector](https://github.com/awslabs/aurora-dsql-connectors/tree/main/java/jdbc)
  on the runtime classpath for `jdbc:aws-dsql:postgresql://` URLs. It is not bundled by this module, so you supply
  it yourself. A plain `jdbc:postgresql://` DSQL endpoint uses the PostgreSQL driver instead and does
  not need the connector, but you must then generate the IAM token yourself and pass it as the
  password.
- AWS credentials on the default provider chain
- IAM permission `dsql:DbConnectAdmin` on the cluster

## Quick Start

### 1. Configure Flyway

```properties
# flyway.conf
flyway.url=jdbc:aws-dsql:postgresql://<CLUSTER_ID>.dsql.<REGION>.on.aws:5432/postgres
flyway.user=admin
flyway.driver=software.amazon.dsql.jdbc.DSQLConnector
```

Both `jdbc:aws-dsql:postgresql://` and `jdbc:postgresql://<host>.dsql.<region>.on.aws` URLs are
recognized. The connector generates the IAM token from the credential chain and parses the region
from the host; no password is supplied.

### 2. Run Migrations

```bash
flyway migrate
```

## Programmatic Usage

```java
Flyway flyway = Flyway.configure()
    .dataSource(
        "jdbc:aws-dsql:postgresql://<CLUSTER_ID>.dsql.<REGION>.on.aws:5432/postgres",
        "admin",
        null)  // Password is null - IAM auth is automatic
    .baselineOnMigrate(true)
    .locations("classpath:db/migration")
    .load();

flyway.migrate();
```

## Writing DSQL-Compatible Migrations

### Index Creation

Use `CREATE INDEX ASYNC` for all indexes:

```sql
CREATE INDEX ASYNC idx_users_email ON users(email);
```

DSQL has no synchronous index creation — see [Asynchronous indexes](#asynchronous-indexes).

### One DDL statement per migration

DSQL accepts a single DDL statement per transaction, with DML in its own. Flyway commits a whole
migration file as one transaction, so give each file one DDL statement:

```sql
-- V1__create_users.sql
CREATE TABLE users (id INT PRIMARY KEY, email VARCHAR(255));
```

To hold several statements in one file, switch off the wrapping transaction for it with a script
configuration file alongside the migration:

```properties
# V2__seed_users.sql.conf
executeInTransaction=false
```

Each statement then commits on its own. Note that OCC retry does not apply to a migration running
outside a transaction.

### Transaction Limits

Be aware of these per-transaction limits when writing migrations:

- Maximum 3,000 rows
- Maximum 10 MiB data size
- Maximum 5 minutes duration

## Configuration

Parameters live under the `flyway.dsql.` namespace, set through any Flyway surface (config file,
CLI flag, environment variable, or the Java API). OCC retry is off by default; the example
below opts in with 3 retries, a reasonable starting point. In `flyway.conf` / `flyway.toml`:

```
flyway.dsql.occMaxRetries=3
flyway.dsql.occMaxRetryDelaySeconds=5
flyway.dsql.awaitAsyncIndexes=false
```

| Parameter | Default | Description |
|---|---|---|
| `occMaxRetries` | `0` | Max retries of a migration transaction that fails with an OCC conflict. `0` disables retries; set a positive value to enable. |
| `occMaxRetryDelaySeconds` | `5` | Upper bound on the exponential backoff between OCC retries. |
| `awaitAsyncIndexes` | `false` | When `true`, wait for `CREATE INDEX ASYNC` builds before the migration returns. |

Equivalent environment variables: `FLYWAY_DSQL_OCC_MAX_RETRIES`,
`FLYWAY_DSQL_OCC_MAX_RETRY_DELAY_SECONDS`, `FLYWAY_DSQL_AWAIT_ASYNC_INDEXES`.

## Asynchronous indexes

`CREATE INDEX ASYNC` returns immediately with a runtime `job_id` and builds the index in the
background, so a plain migration reports success while the index is still building.

With `awaitAsyncIndexes=true`, a SQL migration running `CREATE INDEX ASYNC` captures that `job_id`
and blocks (via `sys.wait_for_job`) until the build finishes; if it fails, the migration fails.

- **Off by default** — fire-and-forget is faster for a pure performance index. Enable the wait when
  a later migration needs the index built, or a failed build should fail the deploy. The wait runs
  after the script finishes, so it gates a later migration, not an earlier statement in the same
  file.
- **SQL migrations only** — Java migrations can capture the `job_id` and call `sys.wait_for_job`
  themselves.
- **A failed build leaves an `INVALID` index** — neither DSQL nor the migration drops it (a timed-out
  wait may front a build that still succeeds). Drop it before rerunning; `IF NOT EXISTS` can silently
  accept the `INVALID` index.

## Not Yet Supported

- `flyway undo` (Flyway Teams feature) — untested with DSQL
- `flyway baseline` — use `baselineOnMigrate=true` instead (see [Troubleshooting](#ddl-and-dml-are-not-supported-in-the-same-transaction))

## Troubleshooting

### "No database found to handle jdbc:aws-dsql:"

Either the Aurora DSQL JDBC connector is not on the classpath (see [Requirements](#requirements)), or
the URL omits the `postgresql://` segment. The connector accepts only
`jdbc:aws-dsql:postgresql://<host>/<database>`.

### "ddl and dml are not supported in the same transaction"

Flyway commits a migration file as one transaction, and DSQL runs one DDL statement per transaction
with DML in its own. This error therefore comes either from a migration file that mixes DDL and DML
(see [One DDL statement per migration](#one-ddl-statement-per-migration)), or from the standalone
`flyway baseline` command, whose create-table and marker insert share a transaction. For baseline,
use `baselineOnMigrate` instead:

```properties
# flyway.conf
flyway.baselineOnMigrate=true
flyway.baselineVersion=1
```

### Token/Authentication Errors

- Verify IAM permissions include `dsql:DbConnectAdmin`
- Check AWS credentials resolve on the default provider chain
- Ensure the region parsed from the endpoint is correct

## License

This project is licensed under the [Apache License 2.0](https://www.apache.org/licenses/LICENSE-2.0).
