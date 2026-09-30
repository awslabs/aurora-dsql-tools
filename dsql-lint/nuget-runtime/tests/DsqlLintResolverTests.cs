using System.Diagnostics;
using System.Runtime.InteropServices;
using Xunit;

namespace Amazon.AuroraDsql.Lint.Runtime.Tests;

public class DsqlLintResolverTests : IDisposable
{
    private readonly string? _originalEnv;

    public DsqlLintResolverTests()
    {
        _originalEnv = Environment.GetEnvironmentVariable(DsqlLintResolver.EnvVariable);
    }

    public void Dispose()
    {
        Environment.SetEnvironmentVariable(DsqlLintResolver.EnvVariable, _originalEnv);
    }

    [Fact]
    public void EnvVar_ValidFile_ReturnsIt()
    {
        var tempFile = CreateTempBinary();
        try
        {
            Environment.SetEnvironmentVariable(DsqlLintResolver.EnvVariable, tempFile);
            var result = DsqlLintResolver.Resolve();
            Assert.Equal(Path.GetFullPath(tempFile), result);
        }
        finally
        {
            File.Delete(tempFile);
        }
    }

    [Fact]
    public void EnvVar_NonExistentFile_ThrowsImmediately()
    {
        var fakePath = Path.Combine(Path.GetTempPath(), "nonexistent-dsql-lint-binary");
        Environment.SetEnvironmentVariable(DsqlLintResolver.EnvVariable, fakePath);

        var ex = Assert.Throws<FileNotFoundException>(DsqlLintResolver.Resolve);
        Assert.Contains(DsqlLintResolver.EnvVariable, ex.Message);
        Assert.Contains(fakePath, ex.Message);
    }

    [Fact]
    public void EnvVar_EmptyString_TreatedAsUnset()
    {
        Environment.SetEnvironmentVariable(DsqlLintResolver.EnvVariable, "");

        // Should not throw about empty env var; proceeds to tier 2/3
        // May throw FileNotFoundException if binary not bundled/on PATH, which is fine
        try
        {
            DsqlLintResolver.Resolve();
        }
        catch (FileNotFoundException ex)
        {
            // Expected if no binary available — just verify it didn't fail on env var
            Assert.DoesNotContain("is set to ''", ex.Message);
        }
    }

    [Fact]
    public void BundledBinary_FoundForCurrentRid()
    {
        Environment.SetEnvironmentVariable(DsqlLintResolver.EnvVariable, null);

        var rid = DsqlLintResolver.GetBaseRid();
        if (rid == null)
        {
            // Skip on unsupported platforms
            return;
        }

        var assemblyDir = Path.GetDirectoryName(typeof(DsqlLintResolver).Assembly.Location)!;
        var exe = RuntimeInformation.IsOSPlatform(OSPlatform.Windows) ? "dsql-lint.exe" : "dsql-lint";
        var nativeDir = Path.Combine(assemblyDir, "runtimes", rid, "native");
        Directory.CreateDirectory(nativeDir);
        var binaryPath = Path.Combine(nativeDir, exe);
        File.WriteAllBytes(binaryPath, []);

        try
        {
            var result = DsqlLintResolver.Resolve();
            Assert.Equal(Path.GetFullPath(binaryPath), result);
        }
        finally
        {
            File.Delete(binaryPath);
        }
    }

    [Fact]
    public void GetBaseRid_ReturnsExpectedFormat()
    {
        var rid = DsqlLintResolver.GetBaseRid();

        Assert.NotNull(rid);
        Assert.Matches(@"^(linux|osx|win)-(x64|arm64)$", rid);
    }

    [Fact]
    public void NothingFound_ThrowsWithAttemptedPaths()
    {
        Environment.SetEnvironmentVariable(DsqlLintResolver.EnvVariable, null);
        // Temporarily override PATH to ensure dsql-lint isn't found
        var originalPath = Environment.GetEnvironmentVariable("PATH");
        Environment.SetEnvironmentVariable("PATH", Path.GetTempPath());

        try
        {
            var ex = Assert.Throws<FileNotFoundException>(DsqlLintResolver.Resolve);
            Assert.Contains("Could not locate the dsql-lint binary", ex.Message);
            Assert.Contains(DsqlLintResolver.EnvVariable, ex.Message);
        }
        finally
        {
            Environment.SetEnvironmentVariable("PATH", originalPath);
        }
    }

    [Fact]
    public void PathScan_FindsBinary()
    {
        Environment.SetEnvironmentVariable(DsqlLintResolver.EnvVariable, null);

        var tempDir = Path.Combine(Path.GetTempPath(), $"dsql-lint-test-{Guid.NewGuid():N}");
        Directory.CreateDirectory(tempDir);
        var exe = RuntimeInformation.IsOSPlatform(OSPlatform.Windows) ? "dsql-lint.exe" : "dsql-lint";
        var binaryPath = Path.Combine(tempDir, exe);
        File.WriteAllBytes(binaryPath, []);

        var originalPath = Environment.GetEnvironmentVariable("PATH");
        var separator = RuntimeInformation.IsOSPlatform(OSPlatform.Windows) ? ";" : ":";
        Environment.SetEnvironmentVariable("PATH", tempDir + separator + originalPath);

        try
        {
            var result = DsqlLintResolver.Resolve();
            Assert.Equal(Path.GetFullPath(binaryPath), result);
        }
        finally
        {
            Environment.SetEnvironmentVariable("PATH", originalPath);
            Directory.Delete(tempDir, true);
        }
    }

    [Fact]
    public void Smoke_ResolvedBinary_RunsVersion()
    {
        string path;
        try
        {
            Environment.SetEnvironmentVariable(DsqlLintResolver.EnvVariable, null);
            path = DsqlLintResolver.Resolve();
        }
        catch (FileNotFoundException)
        {
            // Binary not available in test environment — skip
            return;
        }

        var psi = new ProcessStartInfo(path, "--version")
        {
            RedirectStandardOutput = true,
            UseShellExecute = false,
        };

        using var proc = Process.Start(psi)!;
        proc.WaitForExit();
        Assert.Equal(0, proc.ExitCode);
    }

    private static string CreateTempBinary()
    {
        var path = Path.GetTempFileName();
        File.WriteAllBytes(path, []);
        return path;
    }
}
