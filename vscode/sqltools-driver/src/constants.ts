/*
 * Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
 * SPDX-License-Identifier: MIT
 */

/*
 * This code is based on SQLTools Driver Template
 * Copyright (c) Matheus Teixeira
 * License: MIT
 * Source: https://github.com/mtxr/vsc-sqltools-driver-template/blob/master/src/constants.ts
 */

import { IDriverAlias } from '@sqltools/types';

const { displayName } = require('../../package.json');

export const DRIVER_ALIASES: IDriverAlias[] = [{ displayName: displayName, value: displayName }];
