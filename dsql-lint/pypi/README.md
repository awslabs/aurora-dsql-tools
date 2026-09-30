# dsql-lint

Lint SQL files for [Amazon Aurora DSQL](https://aws.amazon.com/rds/aurora/dsql/) compatibility.

Parses SQL and reports errors (unsupported syntax) with suggested fixes. Includes an auto-fix mode that generates DSQL-compatible SQL.

## Installation

```bash
pip install dsql-lint
```

Or run without installing:

```bash
uvx dsql-lint migration.sql
```

## Usage

```bash
dsql-lint migration.sql [migration2.sql ...]
dsql-lint --format json migration.sql
dsql-lint --fix migration.sql
dsql-lint --version
dsql-lint --help
```

See the [main repository](https://github.com/awslabs/aurora-dsql-tools/tree/main/dsql-lint) for the full rule list, JSON schema, and contribution guide.

## License

MIT-0. See [LICENSE](https://github.com/awslabs/aurora-dsql-tools/blob/main/LICENSE).
