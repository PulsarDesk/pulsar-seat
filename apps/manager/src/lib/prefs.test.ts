// SPDX-License-Identifier: GPL-3.0-only
import { describe, expect, it } from 'vitest';
import { DEFAULT_PREFS, clampFlash, loadPrefs, parsePrefs, savePrefs } from './prefs.ts';

describe('prefs', () => {
  it('falls back to defaults for broken or foreign values', () => {
    expect(parsePrefs(null)).toEqual(DEFAULT_PREFS);
    expect(parsePrefs('{')).toEqual(DEFAULT_PREFS);
    const p = parsePrefs(JSON.stringify({ flashSeconds: 7, theme: 'neon', showIds: 'yes', evenRows: true }));
    expect(p.flashSeconds).toBe(8);
    expect(p.theme).toBe('system');
    expect(p.showIds).toBe(false);
    expect(p.evenRows).toBe(true);
  });

  it('clamps the just-connected duration to 2..20 in steps of 2', () => {
    expect(clampFlash(0)).toBe(2);
    expect(clampFlash(21)).toBe(20);
    expect(clampFlash(5)).toBe(6);
    expect(clampFlash(Number.NaN)).toBe(4);
  });

  it('round-trips through localStorage', () => {
    savePrefs({ ...DEFAULT_PREFS, sharedInSeats: true, tab: 'basic' });
    const p = loadPrefs();
    expect(p.sharedInSeats).toBe(true);
    expect(p.tab).toBe('basic');
  });
});
