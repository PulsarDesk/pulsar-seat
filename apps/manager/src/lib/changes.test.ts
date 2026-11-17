// SPDX-License-Identifier: GPL-3.0-only
import { describe, expect, it } from 'vitest';
import { summarizeChanges } from './changes.ts';
import { demoConfig } from './demo.ts';
import { clone, normalize } from './model.ts';
import { bit } from './mask.ts';

describe('save bar change summary', () => {
  it('counts nothing for an unchanged draft and the pending passwords alone', () => {
    const c = normalize(demoConfig());
    expect(summarizeChanges(c, clone(c))).toEqual({ count: 0, moved: null });
    expect(summarizeChanges(c, clone(c), 2).count).toBe(2);
    expect(summarizeChanges(null, null, 1).count).toBe(1);
  });

  it('names a single device move', () => {
    const c = normalize(demoConfig());
    const d = clone(c);
    const dev = d.devices.find((x) => x.mask !== bit(2) && x.mask !== 0) ?? d.devices[0];
    dev.mask = bit(2);
    const s = summarizeChanges(c, d);
    expect(s.count).toBe(1);
    expect(s.moved).toEqual({ key: dev.key, name: dev.label || dev.name, to: 2 });
    dev.mask = 0;
    expect(summarizeChanges(c, d).moved?.to).toBe('pool');
  });

  it('counts seats and settings groups; no move detail for several changes', () => {
    const c = normalize(demoConfig());
    const d = clone(c);
    d.devices[0].mask = d.devices[0].mask === bit(2) ? bit(1) : bit(2);
    d.seats[1].name = 'Guest';
    d.settings.start_mode = d.settings.start_mode === 'manual' ? 'at_boot' : 'manual';
    const s = summarizeChanges(c, d);
    expect(s.count).toBe(3);
    expect(s.moved).toBeNull();
  });
});
