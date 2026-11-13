// SPDX-License-Identifier: GPL-3.0-only
// Setup wizard helpers (spec 10 §9.2), shared by the main window's wizard view and the
// per-monitor pages. The service owns the capture; these only decide what a page shows and
// what a key pressed in one of our own windows means.

import type { WizardPageSpec } from './api.ts';
import type { DeviceKey, DeviceView, DisplayInfo, SeatNumber, WizardCommand, WizardProgress } from './types.ts';

/** Same numbers as the service (`wizard::MIN_WIDTH` / `MIN_HEIGHT`). */
export const MIN_WIDTH = 1024;
export const MIN_HEIGHT = 600;
/** Seats with a function key (F1..F12, `wizard::fkey_seat`); seats above use "any key". */
export const FKEY_SEATS = 12;
/** Highest monitor number a digit key picks (0 = 10, `wizard::digit`). */
export const DIGIT_MONITORS = 10;

function display(v: DeviceView): DisplayInfo | null {
  return v.device.kind === 'monitor' && v.device.present && v.device.details.type === 'display' ? v.device.details : null;
}

/** The console's primary monitor (desktop origin), else the first one. */
export function primaryMonitor(views: DeviceView[]): DeviceView | null {
  const mons = views.filter((v) => display(v));
  return mons.find((v) => display(v)!.desktop_rect?.x === 0 && display(v)!.desktop_rect?.y === 0) ?? mons[0] ?? null;
}

/**
 * Wizard precondition (spec 10 §9.2: not offered below a minimum resolution). Uses the
 * primary monitor's mode; without a known mode, the screen size of this window.
 */
export function resolutionCheck(views: DeviceView[], screen?: { width: number; height: number; ratio: number }): 'ok' | 'no_monitor' | 'too_low' {
  const p = primaryMonitor(views);
  if (!p) return 'no_monitor';
  const m = display(p)!.mode;
  let w: number;
  let h: number;
  if (m) {
    [w, h] = m.rotation === 90 || m.rotation === 270 ? [m.height, m.width] : [m.width, m.height];
  } else if (screen) {
    w = Math.round(screen.width * screen.ratio);
    h = Math.round(screen.height * screen.ratio);
  } else return 'ok';
  return w >= MIN_WIDTH && h >= MIN_HEIGHT ? 'ok' : 'too_low';
}

/** One page per monitor that is part of the desktop, in the wizard's order. */
export function pageSpecs(views: DeviceView[], order: DeviceKey[]): WizardPageSpec[] {
  const keys = order.length ? order : views.filter((v) => display(v)).map((v) => v.device.key);
  const out: WizardPageSpec[] = [];
  for (const k of keys) {
    const v = views.find((x) => x.device.key === k);
    const r = v ? display(v)?.desktop_rect : undefined;
    if (r && r.width > 0 && r.height > 0) out.push({ monitor: k, x: r.x, y: r.y, width: r.width, height: r.height });
  }
  return out;
}

/** Seat of a monitor in the progress (index in `monitors` + 1), or null (no seat for it). */
export function seatOfMonitor(p: WizardProgress | null, monitor: DeviceKey | null): SeatNumber | null {
  if (!p?.monitors || !monitor) return null;
  const i = p.monitors.indexOf(monitor);
  return i >= 0 ? i + 1 : null;
}

export type PageView =
  | { kind: 'pick'; number: number }
  | { kind: 'keyboard'; seat: SeatNumber; presses: number }
  | { kind: 'mouse'; seat: SeatNumber; clicks: number }
  | { kind: 'other'; seat: SeatNumber; current: SeatNumber }
  | { kind: 'kept'; seat: SeatNumber }
  | { kind: 'done'; seat: SeatNumber }
  | { kind: 'no_seat' };

/** What the page of one seat shows (spec 10 §9.2 steps 2–6). */
export function pageView(p: WizardProgress | null, seat: SeatNumber | null, keepSeatOne: boolean): PageView {
  if (p?.step === 'pick_console_monitor') return seat ? { kind: 'pick', number: seat } : { kind: 'no_seat' };
  if (!seat) return { kind: 'no_seat' };
  if (keepSeatOne && seat === 1) return { kind: 'kept', seat };
  const has = (k: 'kbd' | 'mouse') =>
    !!p?.assigned.some(([s, key]) => s === seat && (k === 'kbd' ? key.startsWith('kbd;') : key.startsWith('mou;')));
  if (p?.confirmed?.includes(seat) || p?.step === 'done' || (has('kbd') && has('mouse'))) return { kind: 'done', seat };
  if (p?.seat === seat) {
    return p.step === 'mouse' ? { kind: 'mouse', seat, clicks: p.clicks } : { kind: 'keyboard', seat, presses: p.key_presses };
  }
  // F<N> works on every page at once; the mouse step follows the order.
  if (!has('kbd')) return { kind: 'keyboard', seat, presses: 0 };
  return { kind: 'other', seat, current: p?.seat ?? seat };
}

export type KeyAction = { type: 'stop'; save: boolean } | { type: 'cmd'; cmd: WizardCommand } | null;

/**
 * A key pressed in one of our wizard windows (spec 10 §9.2 step 6). When the service sees
 * every key of the units itself (`keys_seen`), it acts on them and we do nothing, so a key
 * is never handled twice.
 */
export function keyAction(
  e: { key: string; shiftKey: boolean; altKey: boolean },
  p: WizardProgress | null,
  ownSeat: SeatNumber | null
): KeyAction {
  if (p?.keys_seen) return null;
  const seat = ownSeat ?? p?.seat ?? null;
  if ((e.shiftKey && e.key === 'Escape') || (e.altKey && e.key === 'F4')) return { type: 'stop', save: false };
  if (e.shiftKey && e.key === 'Enter') return { type: 'stop', save: true };
  if (e.shiftKey && e.key === 'Backspace') return { type: 'cmd', cmd: { cmd: 'reset_all' } };
  if (p?.step === 'pick_console_monitor') {
    // Same digits as the service: 1..9, and 0 for monitor 10. Monitors above 10 are
    // picked with the page's button. Enter / Backspace mean nothing before the pick.
    const n = /^[0-9]$/.test(e.key) ? (e.key === '0' ? 10 : Number(e.key)) : 0;
    const k = n && p.monitors ? p.monitors[n - 1] : undefined;
    return k ? { type: 'cmd', cmd: { cmd: 'pick_console_monitor', key: k } } : null;
  }
  if (e.key === 'Enter' && seat) return { type: 'cmd', cmd: { cmd: 'confirm_seat', seat } };
  if (e.key === 'Backspace' && seat) return { type: 'cmd', cmd: { cmd: 'reset_seat', seat } };
  return null;
}

/** A target position in percent of the page, away from the edges and the text. */
export function randomTarget(rand: () => number = Math.random): { x: number; y: number } {
  return { x: 10 + Math.round(rand() * 80), y: 35 + Math.round(rand() * 50) };
}

/** Main keyboard / mouse found by touch (spec 10 §9.1 "gather"): first unit of each kind. */
export function gatherStep(found: { keyboard?: DeviceKey; mouse?: DeviceKey }, key: DeviceKey, views: DeviceView[]): { keyboard?: DeviceKey; mouse?: DeviceKey } {
  const v = views.find((x) => x.device.key === key);
  if (!v || !v.device.present) return found;
  if (v.device.kind === 'keyboard' && !found.keyboard) return { ...found, keyboard: key };
  if (v.device.kind === 'mouse' && !found.mouse) return { ...found, mouse: key };
  return found;
}
