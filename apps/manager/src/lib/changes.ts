// SPDX-License-Identifier: GPL-3.0-only
// What the unsaved draft changes against the saved config, for the docked save bar
// ("2 unsaved changes" + one detail line). One change = one device assignment, one seat or
// one top-level settings group that differs. Pure, so the save bar copy is testable.

import { isShared, isSpare, single } from './mask.ts';
import type { Config, DeviceKey, SeatNumber } from './types.ts';

export interface ChangeSummary {
  count: number;
  /** Set when the only change is one device moving to one seat or to the pool. */
  moved: { key: DeviceKey; name: string; to: SeatNumber | 'pool' } | null;
}

const same = (a: unknown, b: unknown) => JSON.stringify(a ?? null) === JSON.stringify(b ?? null);

export function summarizeChanges(saved: Config | null, draft: Config | null, pendingPasswords = 0): ChangeSummary {
  if (!saved || !draft) return { count: pendingPasswords, moved: null };
  let count = 0;
  let moved: ChangeSummary['moved'] = null;
  let movedCount = 0;

  const before = new Map(saved.devices.map((d) => [d.key, d]));
  const after = new Map(draft.devices.map((d) => [d.key, d]));
  for (const key of new Set([...before.keys(), ...after.keys()])) {
    const a = before.get(key);
    const b = after.get(key);
    if (same(a, b)) continue;
    count++;
    // A device that is new in the draft counts as moved from the pool.
    const fromMask = a?.mask ?? 0;
    if (b && fromMask !== b.mask) {
      movedCount++;
      const seat = single(b.mask);
      if (seat !== null) moved = { key, name: b.label || b.name, to: seat };
      else if (isSpare(b.mask)) moved = { key, name: b.label || b.name, to: 'pool' };
      else if (isShared(b.mask)) moved = null;
    }
  }

  const seatsBefore = new Map(saved.seats.map((x) => [x.number, x]));
  const seatsAfter = new Map(draft.seats.map((x) => [x.number, x]));
  for (const n of new Set([...seatsBefore.keys(), ...seatsAfter.keys()])) {
    if (!same(seatsBefore.get(n), seatsAfter.get(n))) count++;
  }

  const sa = saved.settings as unknown as Record<string, unknown>;
  const sb = draft.settings as unknown as Record<string, unknown>;
  for (const k of new Set([...Object.keys(sa), ...Object.keys(sb)])) {
    if (!same(sa[k], sb[k])) count++;
  }

  count += pendingPasswords;
  return { count, moved: count === 1 && movedCount === 1 ? moved : null };
}
