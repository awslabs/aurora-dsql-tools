/*
 * Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
 * SPDX-License-Identifier: MIT
 */

/*
 * This code is based on SQLTools Driver Template
 * Copyright (c) Matheus Teixeira
 * License: MIT
 * Source: https://github.com/mtxr/vsc-sqltools-driver-template/blob/master/src/ls/plugin.ts
 */

import { ILanguageServerPlugin } from '@sqltools/types';
import AuroraDSQLDriver from './driver';
import { DRIVER_ALIASES } from './../constants';

const AuroraDSQLDriverPlugin: ILanguageServerPlugin = {
  register(server) {
    DRIVER_ALIASES.forEach(({ value }) => {
      server.getContext().drivers.set(value, AuroraDSQLDriver);
    });
  },
};

export default AuroraDSQLDriverPlugin;
