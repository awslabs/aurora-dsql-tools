# Amazon.AuroraDsql.Lint

Lint SQL files for [Amazon Aurora DSQL](https://aws.amazon.com/rds/aurora/dsql/) compatibility.

## Installation

```bash
dotnet tool install -g Amazon.AuroraDsql.Lint
```

## Usage

```bash
dsql-lint migration.sql
dsql-lint --fix migration.sql
dsql-lint < query.sql
```

## Supported Platforms

| OS | Architecture |
|----|-------------|
| Linux | x64, arm64 |
| macOS | x64, arm64 |
| Windows | x64 |

## Links

- [Documentation](https://github.com/awslabs/aurora-dsql-tools/tree/main/dsql-lint)
- [Aurora DSQL SQL Reference](https://docs.aws.amazon.com/aurora-dsql/latest/userguide/working-with-aurora-dsql-sql-reference.html)
