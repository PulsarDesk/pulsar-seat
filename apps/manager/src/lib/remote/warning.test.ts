// SPDX-License-Identifier: GPL-3.0-only
import { describe, expect, it } from 'vitest';
import configRs from '../../../../../crates/seat-core/src/config.rs?raw';
import canonical from '../../../../../crates/seat-session/src/termwrap/warning_tr.txt?raw';
import { WARNING_TR, WARNING_VERSION } from '../warning.ts';

describe('experimental warning text', () => {
  it('is the service text (seat_session::termwrap::WARNING_TR)', () => {
    expect(WARNING_TR).toBe(canonical.replace(/\r\n/g, '\n'));
    expect(WARNING_TR.startsWith('**Deneysel özellik')).toBe(true);
  });

  it('has the version of seat_core::config::EXPERIMENTAL_WARNING_VERSION', () => {
    const m = /EXPERIMENTAL_WARNING_VERSION: u32 = (\d+);/.exec(configRs);
    expect(Number(m?.[1])).toBe(WARNING_VERSION);
  });
});
