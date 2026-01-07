/*
 * Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
 * SPDX-License-Identifier: MIT
 */

/*
 * This code is based on SQLTools PostgreSQL Driver
 * Copyright (c) Matheus Teixeira
 * License: MIT
 * Source: https://github.com/mtxr/vscode-sqltools/blob/dev/packages/driver.pg/src/escape-table.ts
 */

import { NSDatabase } from '@sqltools/types';

export const pgCheckEscape = (w: string | { label: string }) => {
  const identifier = (<any>w).label || w;
  
  if (identifier.length > 63) throw new Error('Identifier too long');
  if (identifier.includes('\0')) throw new Error('Contains null character');
  
  return /[^a-z0-9_]/.test(identifier) 
    ? `"${identifier.replace(/"/g, '""')}"` 
    : identifier;
};

function escapeTableName(table: Partial<NSDatabase.ITable> | string) {
  let items: string[] = [];
  let tableObj = typeof table === 'string' ? <NSDatabase.ITable>{ label: table } : table;
  tableObj.database && items.push(pgCheckEscape(tableObj.database));
  tableObj.schema && items.push(pgCheckEscape(tableObj.schema));
  items.push(pgCheckEscape(tableObj.label));
  return items.join('.');
}

export default escapeTableName;
