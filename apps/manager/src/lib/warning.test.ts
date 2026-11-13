// SPDX-License-Identifier: GPL-3.0-only
import { describe, expect, it } from 'vitest';
import configRs from '../../../../crates/seat-core/src/config.rs?raw';
import canonical from '../../../../crates/seat-session/src/termwrap/warning_tr.txt?raw';
import { EXPERIMENTAL_WARNING_VERSION } from './types.ts';
import { WARNING_TR, WARNING_VERSION } from './warning.ts';

describe('experimental warning', () => {
  it('has a single version constant', () => {
    expect(WARNING_VERSION).toBe(EXPERIMENTAL_WARNING_VERSION);
  });

  it('matches the service text word for word', () => {
    expect(WARNING_TR).toBe(canonical.replace(/\r\n/g, '\n'));
  });

  it('matches the version in seat_core::config', () => {
    const m = /EXPERIMENTAL_WARNING_VERSION: u32 = (\d+);/.exec(configRs);
    expect(Number(m?.[1])).toBe(EXPERIMENTAL_WARNING_VERSION);
  });
});
