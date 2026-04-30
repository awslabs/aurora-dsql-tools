"use strict";

const test = require("node:test");
const assert = require("node:assert/strict");
const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");
const { spawnSync } = require("node:child_process");

const WRAPPER = path.resolve(__dirname, "..", "bin", "dsql-lint");

const PLATFORM_PKGS = {
  "darwin-arm64": "@aws/dsql-lint-darwin-arm64",
  "darwin-x64": "@aws/dsql-lint-darwin-x64",
  "linux-arm64": "@aws/dsql-lint-linux-arm64",
  "linux-x64": "@aws/dsql-lint-linux-x64",
  "win32-x64": "@aws/dsql-lint-win32-x64",
};

const currentPlatformPkg = PLATFORM_PKGS[`${process.platform}-${process.arch}`];
const currentBinaryName = process.platform === "win32" ? "dsql-lint.exe" : "dsql-lint";

function makeTempRoot() {
  return fs.mkdtempSync(path.join(os.tmpdir(), "dsql-lint-wrapper-test-"));
}

// Install a fake platform package for the *current* runner platform, with a
// shim binary at bin/<currentBinaryName>. Returns the tempdir root. The
// wrapper is invoked via a copy placed at <root>/wrapper.js so node's module
// resolution finds <root>/node_modules/<platform-pkg>.
function installFakePlatformPkg(shimContents) {
  if (!currentPlatformPkg) {
    throw new Error(
      `Test runner platform ${process.platform}-${process.arch} is not in PLATFORM_PKGS; ` +
        `cannot install fake platform package. Run tests on a supported platform.`,
    );
  }
  const root = makeTempRoot();
  const pkgDir = path.join(root, "node_modules", currentPlatformPkg);
  const binDir = path.join(pkgDir, "bin");
  fs.mkdirSync(binDir, { recursive: true });
  fs.writeFileSync(
    path.join(pkgDir, "package.json"),
    JSON.stringify({ name: currentPlatformPkg, version: "0.0.0" }),
  );
  const binPath = path.join(binDir, currentBinaryName);
  fs.writeFileSync(binPath, shimContents);
  if (process.platform !== "win32") {
    fs.chmodSync(binPath, 0o755);
  }
  // Copy the wrapper next to node_modules so require.resolve finds the fake.
  fs.copyFileSync(WRAPPER, path.join(root, "wrapper.js"));
  return root;
}

function runWrapper(root, args = [], opts = {}) {
  return spawnSync(process.execPath, [path.join(root, "wrapper.js"), ...args], {
    encoding: "utf-8",
    ...opts,
  });
}

test("missing platform package yields helpful error and non-zero exit", () => {
  const root = makeTempRoot();
  fs.copyFileSync(WRAPPER, path.join(root, "wrapper.js"));
  // No node_modules/@aws/* installed → require.resolve throws in the wrapper.

  const result = runWrapper(root, ["--version"]);
  assert.notEqual(result.status, 0);
  if (currentPlatformPkg) {
    assert.match(result.stderr, /is not installed/);
    assert.match(result.stderr, new RegExp(currentPlatformPkg.replace(/[/\\]/g, "[/\\\\]")));
    assert.match(result.stderr, /--include=optional/);
  } else {
    assert.match(result.stderr, /Unsupported platform/);
  }
});

test("binary missing at resolved path produces ENOENT error with path", () => {
  const root = makeTempRoot();
  const pkgDir = path.join(root, "node_modules", currentPlatformPkg);
  fs.mkdirSync(path.join(pkgDir, "bin"), { recursive: true });
  fs.writeFileSync(
    path.join(pkgDir, "package.json"),
    JSON.stringify({ name: currentPlatformPkg, version: "0.0.0" }),
  );
  // Intentionally do NOT create the binary file.
  fs.copyFileSync(WRAPPER, path.join(root, "wrapper.js"));

  const result = runWrapper(root);
  assert.notEqual(result.status, 0);
  assert.match(result.stderr, /binary not found at/);
});

test("exit code 0 from binary propagates", { skip: process.platform === "win32" }, () => {
  const root = installFakePlatformPkg("#!/bin/sh\nexit 0\n");
  const result = runWrapper(root);
  assert.equal(result.status, 0);
});

test("exit code 1 from binary propagates", { skip: process.platform === "win32" }, () => {
  const root = installFakePlatformPkg("#!/bin/sh\nexit 1\n");
  const result = runWrapper(root);
  assert.equal(result.status, 1);
});

test("exit code 3 from binary propagates (FixedWithWarning case)", {
  skip: process.platform === "win32",
}, () => {
  const root = installFakePlatformPkg("#!/bin/sh\nexit 3\n");
  const result = runWrapper(root);
  assert.equal(result.status, 3);
});

test("stdin is piped through to the binary", { skip: process.platform === "win32" }, () => {
  // Shim reads stdin and writes a sentinel + byte count to stdout so we can
  // verify both directions of stdio: "inherit" without depending on `cat`
  // quoting quirks.
  const root = installFakePlatformPkg(
    "#!/bin/sh\nwc -c | tr -d ' \\n' | awk '{print \"bytes=\" $0}'\n",
  );
  const result = runWrapper(root, [], { input: "CREATE TABLE t (id UUID);" });
  assert.equal(result.status, 0);
  assert.equal(result.stdout.trim(), "bytes=25");
});

test("binary's stderr is forwarded", { skip: process.platform === "win32" }, () => {
  const root = installFakePlatformPkg(
    "#!/bin/sh\necho 'diagnostic from binary' >&2\nexit 1\n",
  );
  const result = runWrapper(root);
  assert.equal(result.status, 1);
  assert.match(result.stderr, /diagnostic from binary/);
});

test("argv is forwarded to the binary", { skip: process.platform === "win32" }, () => {
  const root = installFakePlatformPkg(
    '#!/bin/sh\nprintf \'%s\\n\' "$@"\n',
  );
  const result = runWrapper(root, ["--format", "json", "file.sql"]);
  assert.equal(result.status, 0);
  assert.deepEqual(
    result.stdout.split("\n").filter(Boolean),
    ["--format", "json", "file.sql"],
  );
});
