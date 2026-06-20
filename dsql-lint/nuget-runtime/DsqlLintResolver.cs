using System.Runtime.InteropServices;
using System.Runtime.CompilerServices;

[assembly: InternalsVisibleTo("Amazon.AuroraDsql.Lint.Runtime.Tests")]

namespace Amazon.AuroraDsql.Lint.Runtime;

/// <summary>
/// Resolves the dsql-lint native binary path using a 3-tier strategy:
/// DSQL_LINT_PATH env var → bundled native binary → PATH scan.
/// </summary>
public static class DsqlLintResolver
{
    /// <summary>
    /// Environment variable name checked first during resolution.
    /// </summary>
    public const string EnvVariable = "DSQL_LINT_PATH";

    /// <summary>
    /// Resolves the dsql-lint binary path.
    /// Order: DSQL_LINT_PATH env → bundled native binary → PATH scan.
    /// </summary>
    /// <returns>Absolute path to the dsql-lint executable.</returns>
    /// <exception cref="FileNotFoundException">
    /// Thrown when the binary cannot be found via any resolution tier.
    /// </exception>
    public static string Resolve()
    {
        var attempted = new List<string>();

        var envPath = Environment.GetEnvironmentVariable(EnvVariable);
        if (!string.IsNullOrEmpty(envPath))
        {
            if (File.Exists(envPath))
                return Path.GetFullPath(envPath);

            throw new FileNotFoundException(
                $"Environment variable {EnvVariable} is set to '{envPath}' but the file does not exist.");
        }

        var rid = GetBaseRid();
        var exe = IsWindows() ? "dsql-lint.exe" : "dsql-lint";

        if (rid != null)
        {
            var assemblyLocation = typeof(DsqlLintResolver).Assembly.Location;
            var baseDir = string.IsNullOrEmpty(assemblyLocation)
                ? AppContext.BaseDirectory
                : Path.GetDirectoryName(assemblyLocation)!;

            string[] probePaths =
            [
                Path.Combine(baseDir, "runtimes", rid, "native", exe),
                Path.Combine(baseDir, "..", "runtimes", rid, "native", exe),
                Path.Combine(baseDir, exe),
            ];

            foreach (var path in probePaths)
            {
                if (File.Exists(path))
                {
                    var fullPath = Path.GetFullPath(path);
                    EnsureExecutable(fullPath);
                    return fullPath;
                }
                attempted.Add(Path.GetFullPath(path));
            }
        }

        var pathResult = FindOnPath(exe);
        if (pathResult != null)
            return pathResult;

        attempted.Add($"PATH scan for '{exe}'");

        throw new FileNotFoundException(
            $"Could not locate the dsql-lint binary. Searched:\n" +
            string.Join("\n", attempted.Select(p => $"  - {p}")) +
            $"\n\nTo fix, either:\n" +
            $"  • Set {EnvVariable} to the full path of the dsql-lint binary\n" +
            $"  • Install dsql-lint and ensure it is on your PATH");
    }

    internal static string? GetBaseRid()
    {
        var os = RuntimeInformation.IsOSPlatform(OSPlatform.Windows) ? "win"
               : RuntimeInformation.IsOSPlatform(OSPlatform.OSX) ? "osx"
               : RuntimeInformation.IsOSPlatform(OSPlatform.Linux) ? "linux"
               : null;

        if (os == null)
            return null;

        var arch = RuntimeInformation.OSArchitecture switch
        {
            Architecture.X64 => "x64",
            Architecture.Arm64 => "arm64",
            _ => null
        };

        if (arch == null)
            return null;

        return $"{os}-{arch}";
    }

    private static string? FindOnPath(string exe)
    {
        var pathVar = Environment.GetEnvironmentVariable("PATH");
        if (string.IsNullOrEmpty(pathVar))
            return null;

        var separator = IsWindows() ? ';' : ':';
        foreach (var dir in pathVar.Split(separator, StringSplitOptions.RemoveEmptyEntries))
        {
            var candidate = Path.Combine(dir, exe);
            if (File.Exists(candidate))
                return Path.GetFullPath(candidate);
        }

        return null;
    }

    private static void EnsureExecutable(string path)
    {
        if (OperatingSystem.IsWindows())
            return;

        var mode = File.GetUnixFileMode(path);
        if ((mode & UnixFileMode.UserExecute) == 0)
        {
            File.SetUnixFileMode(path,
                mode | UnixFileMode.UserExecute | UnixFileMode.GroupExecute | UnixFileMode.OtherExecute);
        }
    }

    private static bool IsWindows() =>
        RuntimeInformation.IsOSPlatform(OSPlatform.Windows);
}
