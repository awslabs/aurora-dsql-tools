# Aurora DSQL Tools

[![GitHub](https://img.shields.io/badge/github-awslabs/aurora--dsql--tools-blue?logo=github)](https://github.com/awslabs/aurora-dsql-tools)
[![Discord chat](https://img.shields.io/discord/1435027294837276802.svg?logo=discord)](https://discord.com/invite/nEF6ksFWru)

This monorepo contains developer tools for [Amazon Aurora DSQL](https://aws.amazon.com/rds/aurora/dsql/), AWS's serverless distributed SQL database.

## Available Tools

### VS Code Extensions

| Package | Description | Marketplace |
|---------|-------------|-------------|
| [sqltools-driver](./vscode/sqltools-driver/) | SQLTools driver for Aurora DSQL | [![VS Marketplace](https://img.shields.io/visual-studio-marketplace/v/amazonwebservices.aurora-dsql-driver-for-sqltools)](https://marketplace.visualstudio.com/items?itemName=amazonwebservices.aurora-dsql-driver-for-sqltools) |

### CLI Tools

| Package | Description |
|---------|-------------|
| [dsql-lint](./dsql-lint/) | Lint SQL files for Aurora DSQL compatibility |

### Database Migration Tools

| Package | Description | Maven Central |
|---------|-------------|---------------|
| [flyway](./flyway/) | Flyway database support for Aurora DSQL | [![Maven Central](https://img.shields.io/maven-central/v/software.amazon.dsql/aurora-dsql-flyway-support)](https://central.sonatype.com/artifact/software.amazon.dsql/aurora-dsql-flyway-support) |

## Documentation

See the README in each tool's directory for detailed usage instructions:

- [dsql-lint documentation](./dsql-lint/README.md)
- [SQLTools Driver documentation](./vscode/sqltools-driver/README.md)
- [Flyway support documentation](./flyway/README.md)

## Versioning

Each tool is versioned independently. Version numbers continue from their original standalone repositories to maintain backwards compatibility.

## Contributing

See [CONTRIBUTING.md](./CONTRIBUTING.md) for guidelines on how to contribute to this project.

## Security

See [CONTRIBUTING.md](./CONTRIBUTING.md#security-issue-notifications) for information on reporting security issues.

## License

Each package has its own license:
- dsql-lint: [Apache-2.0](./dsql-lint/LICENSE)
- VS Code SQLTools Driver: [MIT-0](./vscode/sqltools-driver/LICENSE)
- Flyway Support: [Apache-2.0](./flyway/LICENSE)
