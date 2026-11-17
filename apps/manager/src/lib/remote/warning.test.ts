// SPDX-License-Identifier: GPL-3.0-only
import { describe, expect, it } from 'vitest';
import configRs from '../../../../../crates/seat-core/src/config.rs?raw';
import canonicalEn from '../../../../../crates/seat-session/src/termwrap/warning_en.txt?raw';
import canonicalTr from '../../../../../crates/seat-session/src/termwrap/warning_tr.txt?raw';
import { WARNING_EN, WARNING_TR, WARNING_VERSION } from '../warning.ts';

describe('experimental warning text', () => {
  it('is the service text (seat_session::termwrap::WARNING_EN / WARNING_TR)', () => {
    expect(WARNING_EN).toBe(canonicalEn.replace(/\r\n/g, '\n'));
    expect(WARNING_EN.startsWith('**Experimental feature')).toBe(true);
    expect(WARNING_TR).toBe(canonicalTr.replace(/\r\n/g, '\n'));
    expect(WARNING_TR.startsWith('**Deneysel özellik')).toBe(true);
  });

  it('has the version of seat_core::config::EXPERIMENTAL_WARNING_VERSION', () => {
    const m = /EXPERIMENTAL_WARNING_VERSION: u32 = (\d+);/.exec(configRs);
    expect(Number(m?.[1])).toBe(WARNING_VERSION);
  });
});
