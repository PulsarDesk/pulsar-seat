// SPDX-License-Identifier: GPL-3.0-only
import { describe, expect, it } from 'vitest';
import { blocksOverlap, displayNameError, portError, portOverlapError, seatNameError } from './validation.ts';

describe('seatNameError', () => {
  it('accepts ASCII letters and digits', () => {
    for (const ok of ['a', 'Seat1', 'X'.repeat(128)]) expect(seatNameError(ok)).toBeNull();
  });
  it('rejects empty, spaces, symbols, non-ASCII, too long', () => {
    expect(seatNameError('')).toBe('name_empty');
    expect(seatNameError('a b')).toBe('name_chars');
    expect(seatNameError('a-b')).toBe('name_chars');
    expect(seatNameError('çay')).toBe('name_chars');
    expect(seatNameError('x'.repeat(129))).toBe('name_long');
  });
  it('rejects duplicates case-insensitively', () => {
    expect(seatNameError('salon', ['Salon'])).toBe('name_duplicate');
    expect(seatNameError('salon2', ['Salon'])).toBeNull();
  });
});

describe('displayNameError', () => {
  it('rules', () => {
    expect(displayNameError('Salon PC')).toBeNull();
    expect(displayNameError('')).toBe('display_name');
    expect(displayNameError(' a')).toBe('display_name');
    expect(displayNameError('a\nb')).toBe('display_name');
    expect(displayNameError('çalışma odası', ['Çalışma Odası'])).toBe('display_name_duplicate');
    expect(displayNameError('Salon', ['Oda'])).toBeNull();
  });
});

describe('ports', () => {
  it('range 1029-65514', () => {
    expect(portError('1029')).toBeNull();
    expect(portError('65514')).toBeNull();
    expect(portError('1028')).toBe('port');
    expect(portError('65515')).toBe('port');
    expect(portError('abc')).toBe('port');
    expect(portError('')).toBe('port');
  });
  it('blocks of 27 ports must not overlap', () => {
    expect(blocksOverlap(47989, 48015)).toBe(true);
    expect(blocksOverlap(47989, 48016)).toBe(false);
    expect(portOverlapError(48000, [47989])).toBe('port_overlap');
    expect(portOverlapError(48089, [47989])).toBeNull();
  });
});
