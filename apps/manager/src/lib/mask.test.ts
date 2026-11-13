// SPDX-License-Identifier: GPL-3.0-only
import { describe, expect, it } from 'vitest';
import * as M from './mask.ts';
import { SHARED_MASK } from './types.ts';

describe('seat masks', () => {
  it('stays unsigned for seat 16 and shared', () => {
    expect(M.bit(16)).toBe(0x8000);
    expect(M.isShared(-1)).toBe(true);
    expect(M.insert(SHARED_MASK, 3)).toBe(SHARED_MASK);
    expect(M.u32(1 << 31)).toBe(0x80000000);
  });

  it('removing from shared expands to every seat first', () => {
    const m = M.remove(SHARED_MASK, 2);
    expect(M.contains(m, 2)).toBe(false);
    expect(M.contains(m, 1)).toBe(true);
    expect(M.contains(m, 16)).toBe(true);
    expect(M.count(m)).toBe(15);
  });

  it('single, seats, beyond', () => {
    expect(M.single(0b100)).toBe(3);
    expect(M.single(0b110)).toBeNull();
    expect(M.single(SHARED_MASK)).toBeNull();
    expect(M.seatsOf(0b101)).toEqual([1, 3]);
    expect(M.beyond(0b100, 2)).toBe(true);
    expect(M.beyond(SHARED_MASK, 2)).toBe(false);
    expect(M.fromSeats([1, 2])).toBe(3);
    expect(M.coversAll(3, [1, 2])).toBe(true);
    expect(M.coversAll(1, [1, 2])).toBe(false);
  });
});
