// SPDX-License-Identifier: GPL-3.0-only
import { describe, expect, it } from 'vitest';
import { DEFAULT_HOTKEY_SCAN, hotkeyLabel, scanFromCode } from './hotkey.ts';

describe('hotkey', () => {
  it('maps physical keys to set-1 make codes', () => {
    expect(scanFromCode('F12')).toBe(0x58);
    expect(scanFromCode('KeyX')).toBe(0x2d);
    expect(scanFromCode('Digit1')).toBe(0x02);
    expect(scanFromCode('ControlLeft')).toBeNull();
    expect(scanFromCode('ShiftRight')).toBeNull();
    expect(scanFromCode('Unknown')).toBeNull();
  });

  it('labels the default and unknown codes', () => {
    expect(hotkeyLabel(DEFAULT_HOTKEY_SCAN)).toBe('Ctrl + F12');
    expect(hotkeyLabel(0x2d)).toBe('Ctrl + X');
    expect(hotkeyLabel(null)).toBeNull();
    expect(hotkeyLabel(0x7e)).toBe('Ctrl + 0x7e');
  });
});
