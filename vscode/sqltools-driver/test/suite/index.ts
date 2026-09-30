/*
 * Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
 * SPDX-License-Identifier: MIT
 */

/*
 * This code is based on vscode-extension-samples
 * Copyright (c) Microsoft
 * License: MIT
 * Source: https://github.com/microsoft/vscode-extension-samples/blob/main/helloworld-test-sample/src/test/suite/index.ts
 */

import * as path from 'path';
import { glob } from 'glob';

export async function run(): Promise<void> {
  // Mocha 12 is ESM-only. TypeScript's Node16 module mode preserves this
  // dynamic import when emitting the surrounding CommonJS test runner.
  const { default: Mocha } = await import('mocha');
  const testSuite = process.env.TEST_SUITE;
  const testPattern = testSuite ? `**/${testSuite}.test.js` : '**/**.test.js';

  console.log(`Running DSQL driver tests (pattern: ${testPattern})...`);

  const mocha = new Mocha({
    ui: 'tdd',
  });

  const testsRoot = path.resolve(__dirname, '..');

  console.log(`Tests root: ${testsRoot}`);
  return new Promise((c, e) => {
    glob(testPattern, { cwd: testsRoot })
      .then((files) => {
        if (files.length === 0) {
          console.log(`No test files found matching pattern: ${testPattern}`);
        }

        files.forEach((f) => mocha.addFile(path.resolve(testsRoot, f)));

        try {
          mocha.run((failures) => {
            if (failures > 0) {
              e(new Error(`${failures} tests failed.`));
            } else {
              c();
            }
          });
        } catch (err) {
          e(err);
        }
      })
      .catch(e);
  });
}
