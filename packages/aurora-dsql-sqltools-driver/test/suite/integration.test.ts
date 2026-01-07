/*
 * Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
 * SPDX-License-Identifier: MIT
 */
import * as assert from 'assert';
import DSQLDriver from '../../src/ls/driver';
import { ContextValue } from '@sqltools/types';

suite('DSQL Driver Integration Tests', () => {
  let driver: DSQLDriver;

  const mockWorkspace = async () => {
    return [{ name: 'test', uri: '/test' }];
  };

  suiteSetup(() => {
    const clusterEndpoint = process.env.CLUSTER_ENDPOINT;
    const awsRegion = process.env.REGION;

    if (!clusterEndpoint) {
      throw new Error('CLUSTER_ENDPOINT environment variable is required for integration tests');
    }

    console.log(`Using cluster: ${clusterEndpoint}, region: ${awsRegion}`);

    const options = {
      id: 'integration-test',
      isConnected: false,
      isActive: false,
      previewLimit: 50,
      server: clusterEndpoint,
      port: 5432,
      driver: 'SQLTools Aurora DSQL Driver',
      name: 'integration-test-connection',
      database: 'postgres',
      username: 'admin',
      region: awsRegion,
    };

    driver = new DSQLDriver(options, mockWorkspace);
  });

  test('Connect to DSQL cluster', async function () {
    this.timeout(10000);
    await driver.open();
    assert.ok(driver.connection);
  });

  test('List tables', async function () {
    this.timeout(15000);

    try {
      await driver.open();
      const tables = await driver.searchItems(ContextValue.TABLE, '');

      assert.ok(Array.isArray(tables));
    } finally {
      if (driver.connection) {
        await driver.close();
      }
    }
  });

  test('Execute simple query', async function () {
    if (!process.env.CLUSTER_ENDPOINT) {
      this.skip();
    }

    this.timeout(15000);
    const result = await driver.query('SELECT 1 as test_column', {});
    assert.ok(Array.isArray(result));
    assert.ok(result.length > 0);
  });

  suiteTeardown(async () => {
    if (driver && driver.connection) {
      await driver.close();
    }
  });
});
