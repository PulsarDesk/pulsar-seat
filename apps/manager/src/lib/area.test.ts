// SPDX-License-Identifier: GPL-3.0-only
// Split screen (G8): grid <-> areas, the seat-core `check_split` mirror and the config edit.
import { describe, expect, it } from 'vitest';
import { areaFromCells, checkSplit, gridOf, seatAreaOf, splitFromGrid, splitIssueCode } from './area.ts';
import * as M from './mask.ts';
import { localIssues, newSeat, normalize, setDeviceMask, setMonitorOutputs, setMonitorSplit } from './model.ts';
import { demoConfig } from './demo.ts';
import type { Config, Device } from './types.ts';

const mon: Device = {
  key: 'mon;loc=PCIROOT(0);sub=t1',
  kind: 'monitor',
  name: 'Monitor',
  present: true,
  warnings: [],
  details: { type: 'display' }
} as unknown as Device;

function cfg(): Config {
  const c = normalize(demoConfig());
  for (const n of [2, 3, 4, 5]) if (!c.seats.some((s) => s.number === n)) c.seats.push(newSeat(n));
  c.devices = c.devices.filter((d) => d.key !== mon.key);
  return normalize(c);
}

describe('split screen areas', () => {
  it('maps cells to areas and back', () => {
    expect(areaFromCells(0b0101)).toBe('left');
    expect(areaFromCells(0b1001)).toBeNull();
    expect(gridOf([{ seat: 2, area: 'left' }, { seat: 3, area: 'top_right' }])).toEqual([2, 3, 2, 0]);
    expect(gridOf(undefined)).toEqual([0, 0, 0, 0]);
  });

  it('reads a grid like seat-core', () => {
    expect(splitFromGrid([0, 0, 0, 0]).kind).toBe('none');
    expect(splitFromGrid([2, 2, 2, 2]).kind).toBe('none');
    expect(splitFromGrid([2, 3, 2, 3])).toEqual({
      kind: 'split',
      areas: [
        { seat: 2, area: 'left' },
        { seat: 3, area: 'right' }
      ],
      mask: M.fromSeats([2, 3])
    });
    expect(splitFromGrid([2, 3, 4, 5]).kind).toBe('split');
    // Diagonal cells are no shape; seat 1 only half and half with one seat.
    expect(splitFromGrid([2, 3, 3, 2])).toEqual({ kind: 'invalid', why: 'shape' });
    expect(splitFromGrid([1, 2, 1, 3])).toEqual({ kind: 'invalid', why: 'console_share_not_half' });
    expect(splitFromGrid([1, 1, 2, 2]).kind).toBe('split');
  });

  it('mirrors check_split', () => {
    const m23 = M.fromSeats([2, 3]);
    expect(checkSplit('monitor', m23, {})).toBe(false);
    expect(checkSplit('monitor', m23, { areas: [{ seat: 2, area: 'left' }, { seat: 3, area: 'right' }] })).toBe(true);
    expect(checkSplit('monitor', m23, { areas: [{ seat: 2, area: 'left' }, { seat: 3, area: 'top_left' }] })).toBe('overlap');
    expect(checkSplit('monitor', m23, { areas: [{ seat: 2, area: 'left' }] })).toBe('area_mismatch');
    expect(checkSplit('keyboard', m23, { areas: [{ seat: 2, area: 'left' }] })).toBe('not_monitor');
    expect(checkSplit('monitor', M.bit(2), { areas: [{ seat: 2, area: 'left' }] })).toBe(false);
    expect(checkSplit('monitor', m23, { areas: [{ seat: 2, area: 'left' }, { seat: 3, area: 'right' }], outputs: [1] })).toBe('clone_on_split');
    expect(splitIssueCode('overlap')).toBe('screen_area_overlap');
    expect(splitIssueCode('area_mismatch')).toBe('invalid_screen_area');
    expect(seatAreaOf('monitor', m23, { areas: [{ seat: 2, area: 'left' }, { seat: 3, area: 'right' }] }, 3)).toBe('right');
  });

  it('edits the config and validates like the service', () => {
    let c = setMonitorOutputs(cfg(), mon, [7], M.bit(2));
    c = setMonitorSplit(c, mon, [2, 3, 2, 3], M.bit(2));
    const a = c.devices.find((d) => d.key === mon.key)!;
    expect(a.mask).toBe(M.fromSeats([2, 3]));
    expect(a.options?.areas).toHaveLength(2);
    expect(a.options?.outputs).toBeUndefined();
    const codes = (x: Config) => localIssues(x, null).filter((i) => i.device === mon.key).map((i) => i.code);
    expect(codes(c)).toEqual([]);
    // A broken split is reported once, on its areas.
    const broken = structuredClone(c);
    broken.devices.find((d) => d.key === mon.key)!.options!.areas![1].area = 'top_left';
    expect(codes(broken)).toEqual(['screen_area_overlap']);
    // Invalid grid: no change. One seat left: the split ends.
    expect(setMonitorSplit(c, mon, [2, 3, 3, 2], M.bit(2))).toBe(c);
    const one = setMonitorSplit(c, mon, [3, 0, 0, 0], M.bit(2)).devices.find((d) => d.key === mon.key)!;
    expect([one.mask, one.options?.areas]).toEqual([M.bit(3), undefined]);
    // Moving a split monitor to one seat ends the split too.
    const moved = setDeviceMask(c, mon, M.bit(4)).devices.find((d) => d.key === mon.key)!;
    expect([moved.mask, moved.options]).toEqual([M.bit(4), undefined]);
  });
});
