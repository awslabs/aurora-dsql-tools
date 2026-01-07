/*
 * Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
 * SPDX-License-Identifier: MIT
 */

/*
 * This code is based on vscode-extension-samples
 * Copyright (c) Microsoft
 * License: MIT
 * Source: https://github.com/microsoft/vscode-extension-samples/blob/main/helloworld-test-sample/src/test/runTest.ts
 */

import * as path from 'path';
import { runTests, downloadAndUnzipVSCode, resolveCliArgsFromVSCodeExecutablePath } from '@vscode/test-electron';
import { execSync } from 'child_process';

async function main() {
  try {
    // Download VS Code
    const vscodeExecutablePath = await downloadAndUnzipVSCode();

    // Install SQLTools extension
    const [cli, ...args] = resolveCliArgsFromVSCodeExecutablePath(vscodeExecutablePath);
    const installCmd = `"${cli}" ${args.join(' ')} --install-extension mtxr.sqltools --force`;

    console.log('Installing SQLTools extension...');
    execSync(installCmd, { stdio: 'inherit' });
    console.log('SQLTools installed!');

    // Run tests
    const extensionDevelopmentPath = path.resolve(__dirname, '../../');
    const extensionTestsPath = path.resolve(__dirname, './suite/index');

    await runTests({
      vscodeExecutablePath,
      extensionDevelopmentPath,
      extensionTestsPath,
    });
  } catch (err) {
    console.error('Failed to run tests:', err);
    process.exit(1);
  }
}

main();
