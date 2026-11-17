// SPDX-License-Identifier: GPL-3.0-only
// Seat-level camera indication (spec/local 10 §3.5.1, 70 "all cameras at once"): which
// webcams a grid shows, and how a device name finds its WebView video input.

import * as M from './mask.ts';
import type { DeviceView, SeatNumber } from './types.ts';

/** Names of the present webcams, all of them or those `seat` may use. */
export function cameraNames(devices: DeviceView[], seat: SeatNumber | null = null): string[] {
  return devices
    .filter((v) => v.device.kind === 'webcam' && v.device.present && (seat === null || M.contains(v.effective, seat)))
    .map((v) => v.device.name);
}

/** Best match of a device friendly name among the media device labels (exact, then contains). */
export function matchCamera<T extends { label: string }>(list: T[], name: string): T | undefined {
  const n = name.toLowerCase();
  return (
    list.find((d) => d.label.toLowerCase() === n) ??
    list.find((d) => d.label && (d.label.toLowerCase().includes(n) || n.includes(d.label.toLowerCase())))
  );
}

/**
 * Video inputs a grid opens: with `names`, one per name that matches (in that order, no
 * duplicates); without, every video input.
 */
export function gridInputs<T extends { label: string; deviceId: string }>(inputs: T[], names: string[] | null): T[] {
  if (!names) return inputs;
  const out: T[] = [];
  for (const n of names) {
    const m = matchCamera(inputs, n);
    if (m && !out.some((x) => x.deviceId === m.deviceId)) out.push(m);
  }
  return out;
}
