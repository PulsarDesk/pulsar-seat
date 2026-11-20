// SPDX-License-Identifier: GPL-3.0-only
// Split screen (OSMS-parity G8, 2026-11-20): mirror of `seat_core::area`. A monitor is cut
// into a 2x2 grid of cells; up to four seats each take a half or a quarter of it. The
// manager edits a split as four cells (top left, top right, bottom left, bottom right), each
// naming a seat or nobody; the cells of one seat must form a half or a quarter.

import * as M from './mask.ts';
import type { AssignmentOptions, DeviceKind, ScreenArea, SeatArea, SeatMask, SeatNumber } from './types.ts';

export const MAX_SEATS_PER_MONITOR = 4;

/** Cell bits in grid order: top left, top right, bottom left, bottom right. */
export const CELL_BITS = [0b0001, 0b0010, 0b0100, 0b1000] as const;

export const AREA_CELLS: Record<ScreenArea, number> = {
  full: 0b1111,
  left: 0b0101,
  right: 0b1010,
  top: 0b0011,
  bottom: 0b1100,
  top_left: 0b0001,
  top_right: 0b0010,
  bottom_left: 0b0100,
  bottom_right: 0b1000
};

export function areaFromCells(cells: number): ScreenArea | null {
  for (const [a, c] of Object.entries(AREA_CELLS) as [ScreenArea, number][]) if (c === cells) return a;
  return null;
}

export const isHalf = (a: ScreenArea): boolean => a === 'left' || a === 'right' || a === 'top' || a === 'bottom';

const OPPOSITE: Partial<Record<ScreenArea, ScreenArea>> = { left: 'right', right: 'left', top: 'bottom', bottom: 'top' };

/** Seat per cell (0 = nobody), grid order. */
export type SplitGrid = [number, number, number, number];

/** The cells of a saved split (empty grid when the monitor is not split). */
export function gridOf(areas: SeatArea[] | undefined): SplitGrid {
  const g: SplitGrid = [0, 0, 0, 0];
  for (const a of areas ?? []) {
    const cells = AREA_CELLS[a.area] ?? 0;
    CELL_BITS.forEach((b, i) => {
      if (cells & b) g[i] = a.seat;
    });
  }
  return g;
}

/** Why a split is refused (seat-core `SplitIssue`). */
export type SplitIssue = 'not_monitor' | 'shared' | 'too_many_seats' | 'area_mismatch' | 'overlap' | 'clone_on_split' | 'console_share_not_half';

/** Validation code for a split issue (seat-core `validate`). */
export function splitIssueCode(why: SplitIssue): 'screen_area_overlap' | 'console_share_not_half' | 'invalid_screen_area' {
  if (why === 'overlap') return 'screen_area_overlap';
  if (why === 'console_share_not_half') return 'console_share_not_half';
  return 'invalid_screen_area';
}

/** Mirror of `seat_core::area::check_split`: `true` = a valid split, `false` = not split. */
export function checkSplit(kind: DeviceKind, mask: SeatMask, options: AssignmentOptions | undefined): boolean | SplitIssue {
  const areas = options?.areas ?? [];
  if (areas.length === 0) return false;
  if (kind !== 'monitor') return 'not_monitor';
  if (M.isShared(mask)) return 'shared';
  if (M.count(mask) < 2) return false;
  if (M.count(mask) > MAX_SEATS_PER_MONITOR || areas.length > MAX_SEATS_PER_MONITOR) return 'too_many_seats';
  const seen = new Set<SeatNumber>();
  for (const a of areas) {
    if (!M.contains(mask, a.seat) || seen.has(a.seat)) return 'area_mismatch';
    seen.add(a.seat);
  }
  if (seen.size !== M.count(mask)) return 'area_mismatch';
  let used = 0;
  for (const a of areas) {
    const c = AREA_CELLS[a.area] ?? 0;
    if (used & c) return 'overlap';
    used |= c;
  }
  if ((options?.outputs?.length ?? 0) > 0) return 'clone_on_split';
  const con = areas.find((a) => a.seat === 1);
  if (con) {
    const ok = areas.length === 2 && isHalf(con.area) && areas.some((a) => a.seat !== 1 && a.area === OPPOSITE[con.area]);
    if (!ok) return 'console_share_not_half';
  }
  return true;
}

/** Result of reading a grid: not split (one seat or none), a split, or why it is refused. */
export type GridSplit = { kind: 'none' } | { kind: 'split'; areas: SeatArea[]; mask: SeatMask } | { kind: 'invalid'; why: 'shape' | 'console_share_not_half' };

/** The split a 2x2 grid describes. Each seat's cells must form a half or a quarter. */
export function splitFromGrid(grid: SplitGrid): GridSplit {
  const cells = new Map<SeatNumber, number>();
  grid.forEach((seat, i) => {
    if (seat > 0) cells.set(seat, (cells.get(seat) ?? 0) | CELL_BITS[i]);
  });
  if (cells.size < 2) return { kind: 'none' };
  const areas: SeatArea[] = [];
  for (const [seat, c] of [...cells].sort((a, b) => a[0] - b[0])) {
    const area = areaFromCells(c);
    if (!area || area === 'full') return { kind: 'invalid', why: 'shape' };
    areas.push({ seat, area });
  }
  const mask = M.fromSeats(areas.map((a) => a.seat));
  if (checkSplit('monitor', mask, { areas }) === 'console_share_not_half') return { kind: 'invalid', why: 'console_share_not_half' };
  return { kind: 'split', areas, mask };
}

/** This seat's part of a monitor whose saved assignment is a valid split. */
export function seatAreaOf(kind: DeviceKind, mask: SeatMask, options: AssignmentOptions | undefined, seat: SeatNumber): ScreenArea | null {
  if (checkSplit(kind, mask, options) !== true) return null;
  return options?.areas?.find((a) => a.seat === seat)?.area ?? null;
}
