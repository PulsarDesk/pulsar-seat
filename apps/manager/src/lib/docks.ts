// SPDX-License-Identifier: GPL-3.0-only
// USB dock wizard (spec/local 10 §8.8, compat-tweaks 2026-11-17): give each multi-seat USB
// dock (monitor + sound + USB ports in one box) to one seat in one step. The general
// device assignment does the work; this only applies it to every device of the dock.

import * as M from './mask.ts';
import type { UsbDock } from './compat.ts';
import { addSeat, renameSeat, setDeviceMask, setKeyMask } from './model.ts';
import type { Config, SeatNumber } from './types.ts';

/** What the wizard does with one dock: a seat, a new seat, or nothing. */
export type DockChoice = { kind: 'seat'; seat: SeatNumber } | { kind: 'new' } | { kind: 'skip' };

/** Assign every dock to its chosen seat (new seats are added first, optionally named).
 *  The dock's hub (container or port key) and each of its monitors, input and sound
 *  devices get the seat; nothing else changes. Returns the new draft and how many docks
 *  were assigned (a dock that needs a new seat beyond the limit is skipped). */
export function assignDocks(c: Config, docks: UsbDock[], choices: DockChoice[], names: string[] = []): { config: Config; assigned: number } {
  let out = c;
  let assigned = 0;
  docks.forEach((dock, i) => {
    const choice = choices[i] ?? { kind: 'skip' };
    let seat: SeatNumber | null = null;
    if (choice.kind === 'seat') seat = choice.seat;
    if (choice.kind === 'new') {
      const r = addSeat(out, names[i]);
      if (!r) return;
      out = r.config;
      seat = r.seat;
    } else if (seat !== null && names[i]?.trim()) {
      out = renameSeat(out, seat, names[i]);
    }
    if (seat === null) return;
    const mask = M.bit(seat);
    out = setKeyMask(out, dock.key, dock.key.startsWith('cont;') ? 'other' : 'usb_hub', mask, dock.hub.name);
    for (const d of [...dock.monitors, ...dock.inputs, ...dock.audio]) out = setDeviceMask(out, d, mask);
    assigned += 1;
  });
  return { config: out, assigned };
}
