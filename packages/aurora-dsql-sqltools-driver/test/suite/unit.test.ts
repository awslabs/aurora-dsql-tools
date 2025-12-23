/*
 * Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
 * SPDX-License-Identifier: MIT
 */
import * as assert from "assert";
import DSQLDriver from "../../src/ls/driver";
import { pgCheckEscape } from "../../src/escape-table";

suite("DSQL Driver Unit Tests", () => {
  const mockWorkspace = async () => {
    return [{ name: "test", uri: "/test" }];
  };

  test("Driver instantiation", () => {
    const options = {
      id: "test",
      isConnected: false,
      isActive: false,
      previewLimit: 50,
      server: "test.dsql.us-east-1.on.aws",
      port: 5432,
      driver: "SQLTools Aurora DSQL Driver",
      name: "test-connection",
      database: "postgres",
      username: "admin",
      region: "us-east-1"
    };

    const driver = new DSQLDriver(options, mockWorkspace);

    assert.ok(driver);
    assert.strictEqual(driver.credentials.server, "test.dsql.us-east-1.on.aws");
    assert.strictEqual(driver.credentials.region, "us-east-1");
  });

  test("SSL configuration processing", () => {
    const options = {
      id: "test-ssl",
      isConnected: false,
      isActive: false,
      previewLimit: 50,
      server: "test.dsql.us-east-1.on.aws",
      port: 5432,
      driver: "SQLTools Aurora DSQL Driver",
      name: "test-ssl-connection",
      database: "postgres",
      username: "admin",
      region: "us-east-1",
      pgOptions: {
        ssl: {
          rejectUnauthorized: true,
          ca: "test-ca-content"
        }
      }
    };

    const driver = new DSQLDriver(options, mockWorkspace);

    assert.ok(driver.credentials.pgOptions);
    assert.ok(driver.credentials.pgOptions.ssl);
    if (typeof driver.credentials.pgOptions.ssl === 'object') {
      assert.strictEqual(driver.credentials.pgOptions.ssl.rejectUnauthorized, true);
    }
  });

  suite("pgCheckEscape function", () => {
    test("Simple identifiers without special characters", () => {
      assert.strictEqual(pgCheckEscape("users"), "users");
      assert.strictEqual(pgCheckEscape("user_table"), "user_table");
      assert.strictEqual(pgCheckEscape("table123"), "table123");
    });

    test("Identifiers with special characters get quoted", () => {
      assert.strictEqual(pgCheckEscape("user table"), '"user table"');
      assert.strictEqual(pgCheckEscape("user-table"), '"user-table"');
      assert.strictEqual(pgCheckEscape("UserTable"), '"UserTable"');
      assert.strictEqual(pgCheckEscape("user@domain"), '"user@domain"');
    });

    test("Quote escaping prevents SQL injection", () => {
      assert.strictEqual(pgCheckEscape('my"table'), '"my""table"');
      assert.strictEqual(pgCheckEscape('table"; DROP TABLE users; --'), '"table""; DROP TABLE users; --"');
      assert.strictEqual(pgCheckEscape('""'), '""""""');  // Two quotes become four quotes, plus outer quotes = 6 total
    });

    test("Object with label property", () => {
      assert.strictEqual(pgCheckEscape({ label: "users" }), "users");
      assert.strictEqual(pgCheckEscape({ label: "user table" }), '"user table"');
      assert.strictEqual(pgCheckEscape({ label: 'my"table' }), '"my""table"');
    });

    test("Length validation", () => {
      const longIdentifier = "a".repeat(64);
      assert.throws(() => pgCheckEscape(longIdentifier), /Identifier too long/);
      
      const maxLengthIdentifier = "a".repeat(63);
      assert.strictEqual(pgCheckEscape(maxLengthIdentifier), maxLengthIdentifier);
    });

    test("Null character validation", () => {
      assert.throws(() => pgCheckEscape("table\0name"), /Contains null character/);
      assert.throws(() => pgCheckEscape("table\x00name"), /Contains null character/);
    });

    test("Edge cases", () => {
      assert.strictEqual(pgCheckEscape("a"), "a");
      assert.strictEqual(pgCheckEscape("_"), "_");
      assert.strictEqual(pgCheckEscape("9table"), "9table"); // starts with number but regex only checks for non a-z0-9_
      assert.strictEqual(pgCheckEscape("table$"), '"table$"'); // contains dollar sign
    });
  });
});
