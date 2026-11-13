// SPDX-License-Identifier: GPL-3.0-only
import { describe, expect, it } from 'vitest';
import rustSource from '../../../../crates/split-termwrap/src/lib.rs?raw';
import { WARNING_TR, WARNING_VERSION } from './warning.ts';

describe('experimental warning text', () => {
  it('is identical to split_termwrap::WARNING_TR and has the same version', () => {
    const src = rustSource.replace(/\r\n/g, '\n');
    const head = 'pub const WARNING_TR: &str = "\\\n';
    const start = src.indexOf(head);
    expect(start).toBeGreaterThan(0);
    const body = src.slice(start + head.length, src.indexOf('";', start));
    expect(WARNING_TR).toBe(body);
    expect(src).toContain(`pub const WARNING_VERSION: u32 = ${WARNING_VERSION};`);
  });
});
