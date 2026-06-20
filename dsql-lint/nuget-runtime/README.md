# Amazon.AuroraDsql.Lint.Runtime

Runtime resolver for the [dsql-lint](https://github.com/awslabs/aurora-dsql-tools/tree/main/dsql-lint) native binary. Enables .NET libraries (e.g. an EF Core adapter) to locate and spawn `dsql-lint` via `PackageReference`.

## Installation

```xml
<PackageReference Include="Amazon.AuroraDsql.Lint.Runtime" />
```

## Usage

```csharp
using Amazon.AuroraDsql.Lint.Runtime;

var binaryPath = DsqlLintResolver.Resolve();
// Use binaryPath with System.Diagnostics.Process
```

## Resolution Strategy

The resolver uses a 3-tier strategy (mirrors Prisma's engine resolution):

1. **`DSQL_LINT_PATH` environment variable** — explicit override for CI, custom builds, or testing
2. **Bundled binary** — from the NuGet package's `runtimes/<rid>/native/` directory
3. **PATH scan** — fallback to a globally installed `dsql-lint`

## MSBuild Integration

The package exposes a `$(DsqlLintExe)` MSBuild property for use in custom build tasks:

```xml
<Target Name="LintMigrations" BeforeTargets="Build">
  <Exec Command="$(DsqlLintExe) Migrations/" />
</Target>
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
