using System.Diagnostics;
using System.Runtime.InteropServices;

var rid = GetRid();
var binary = ResolveBinary(rid);

if (!OperatingSystem.IsWindows())
{
    File.SetUnixFileMode(binary,
        UnixFileMode.UserRead | UnixFileMode.UserWrite | UnixFileMode.UserExecute |
        UnixFileMode.GroupRead | UnixFileMode.GroupExecute |
        UnixFileMode.OtherRead | UnixFileMode.OtherExecute);
}

var psi = new ProcessStartInfo(binary) { UseShellExecute = false };
foreach (var arg in args)
    psi.ArgumentList.Add(arg);

using var proc = Process.Start(psi)
    ?? throw new InvalidOperationException($"Failed to start process: {binary}");
proc.WaitForExit();
return proc.ExitCode;

static string GetRid()
{
    var os = RuntimeInformation.IsOSPlatform(OSPlatform.Windows) ? "win"
           : RuntimeInformation.IsOSPlatform(OSPlatform.OSX) ? "osx"
           : RuntimeInformation.IsOSPlatform(OSPlatform.Linux) ? "linux"
           : throw new PlatformNotSupportedException("Unsupported OS");

    var arch = RuntimeInformation.OSArchitecture switch
    {
        Architecture.X64 => "x64",
        Architecture.Arm64 => "arm64",
        _ => throw new PlatformNotSupportedException(
            $"Unsupported architecture: {RuntimeInformation.OSArchitecture}")
    };

    var rid = $"{os}-{arch}";
    string[] supported = ["linux-arm64", "linux-x64", "osx-arm64", "osx-x64", "win-x64"];
    if (!supported.Contains(rid))
        throw new PlatformNotSupportedException(
            $"Unsupported platform: {rid}. Supported: {string.Join(", ", supported)}");

    return rid;
}

static string ResolveBinary(string rid)
{
    var exe = OperatingSystem.IsWindows() ? "dsql-lint.exe" : "dsql-lint";
    var baseDir = AppContext.BaseDirectory;

    string[] probePaths =
    [
        Path.Combine(baseDir, "runtimes", rid, "native", exe),
        Path.Combine(baseDir, "..", "runtimes", rid, "native", exe),
    ];

    foreach (var path in probePaths)
    {
        if (File.Exists(path))
            return Path.GetFullPath(path);
    }

    throw new FileNotFoundException(
        $"dsql-lint binary not found for {rid}. Searched:\n" +
        string.Join("\n", probePaths.Select(p => $"  {Path.GetFullPath(p)}")));
}
