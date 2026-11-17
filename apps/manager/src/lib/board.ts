// SPDX-License-Identifier: GPL-3.0-only
// Pure helpers of the Seats board (redesign P3, target/redesign/final DESIGN.md "Koltuklar"):
// seat titles and status words, the status line summary, the device tree order, unsaved moves
// and the missing-device lines. No Svelte here, so the rules are unit-tested on their own.

import { productLabel, seatLabel, t } from './i18n.ts';
import { effectiveMasks, type CanvasItem, type SeatHealth } from './model.ts';
import type { IconName } from './ui/icons.ts';
import type { StatusKind } from './ui/types.ts';
import type { ActiveBackends, Config, Device, DeviceKey, DeviceKind, DeviceView, ProductState, SeatConfig, SeatNumber, SeatRuntime, SeatState } from './types.ts';

/** The card title: the seat's own name, or "Seat 2" while it has none. */
export function seatTitle(s: Pick<SeatConfig, 'number' | 'name'>): string {
  return s.name.trim() && s.name !== String(s.number) ? s.name : t.seatN(s.number);
}

/** Whether the seat is named (its card then carries "Seat N" in the fact line). */
export function isNamed(s: Pick<SeatConfig, 'number' | 'name'>): boolean {
  return seatTitle(s) !== t.seatN(s.number);
}

/** A seat session is up (sign-in screen, signed in or locked). */
export function isUp(state: SeatState | undefined): boolean {
  return !!state && (state.state === 'at_logon' || state.state === 'signed_in' || state.state === 'locked');
}

/** A seat cannot start: no screen or no keyboard (spec 10 §3.3 red / yellow). */
export function isBlocked(health: SeatHealth): boolean {
  return health.readiness === 'no_display' || health.readiness === 'no_keyboard';
}

export interface SeatStatusWord {
  kind: StatusKind;
  text: string;
}

/**
 * The one status of a seat card (Words-First Rule): restart required wins, then the live
 * state (up = ok "Running", error = danger, starting / stopping = neutral with the state word),
 * then the configured health (cannot start = warn), else neutral "Off".
 */
export function seatStatus(rt: SeatRuntime | undefined, health: SeatHealth, restart: boolean): SeatStatusWord {
  if (restart) return { kind: 'warn', text: t.seatCard.restartNeeded };
  const st = rt?.state;
  if (st && isUp(st)) return { kind: 'ok', text: t.seatCard.running };
  if (st?.state === 'error') return { kind: 'danger', text: t.seatState.error };
  if (st && st.state !== 'off') return { kind: 'off', text: t.seatState[st.state] ?? st.state };
  if (isBlocked(health)) return { kind: 'warn', text: t.seatCard.blocked };
  return { kind: 'off', text: t.seatCard.off };
}

/**
 * The muted fact after the status: what the session shows (signed-in user, the sign-in
 * screen, the failure), else the auto sign-in account. A named seat leads with "Seat N".
 */
export function seatFact(s: SeatConfig, rt: SeatRuntime | undefined): string {
  const parts: string[] = [];
  if (isNamed(s)) parts.push(t.seatN(s.number));
  const st = rt?.state;
  if (st?.state === 'at_logon') parts.push(t.seatCard.atLogon);
  else if (st?.state === 'signed_in' || st?.state === 'locked' || st?.state === 'error') parts.push(seatLabel(st));
  else if (st?.state === 'starting') parts.push(seatLabel(st));
  else if (s.account.kind !== 'logon_screen') parts.push(s.account.kind === 'domain' ? `${s.account.domain}\\${s.account.user}` : s.account.user);
  return parts.join(' · ');
}

export interface PowerLine {
  kind: StatusKind;
  status: string;
  summary: string;
  /** Which compact action the line offers. */
  action: 'start' | 'enable' | null;
}

/** The status line under the Seats title (DESIGN.md "Per screen: Koltuklar"). */
export function powerLine(
  product: ProductState | undefined,
  seats: SeatConfig[],
  runtime: (n: SeatNumber) => SeatRuntime | undefined,
  health: (n: SeatNumber) => SeatHealth
): PowerLine | null {
  if (!product) return null;
  if (product.state === 'safe_mode') return { kind: 'warn', status: productLabel(product), summary: '', action: null };
  if (product.state === 'disabled') {
    return { kind: 'off', status: t.power.off, summary: seats.length > 1 ? t.power.offSummary : '', action: 'enable' };
  }
  const local = seats.filter((s) => (s.kind ?? 'local') === 'local');
  const running = local.filter((s) => isUp(runtime(s.number)?.state)).length;
  const blocked = local
    .filter((s) => (s.autostart || s.number === 1) && !isUp(runtime(s.number)?.state) && isBlocked(health(s.number)))
    .map(seatTitle);
  const summary = local.length <= 1 ? t.power.firstOnly : t.power.summary(running, blocked.join(', '));
  return { kind: 'ok', status: t.power.on, summary, action: 'start' };
}

export interface Placed {
  item: CanvasItem;
  /** Hub indentation (0 = top level); a device sits under its hub when the hub is listed too. */
  depth: number;
}

/**
 * Tree order of a device list: each device follows the hub it is plugged into when that hub
 * is in the same list (any depth); the rest keep the canvas order (category, then arrival).
 */
export function treeOrder(items: CanvasItem[]): Placed[] {
  const keys = new Set(items.map((i) => i.view.device.key));
  const kids = new Map<DeviceKey, CanvasItem[]>();
  const roots: CanvasItem[] = [];
  for (const i of items) {
    const p = i.view.device.parent_hub;
    if (p && keys.has(p) && p !== i.view.device.key) {
      const list = kids.get(p) ?? [];
      list.push(i);
      kids.set(p, list);
    } else roots.push(i);
  }
  const out: Placed[] = [];
  const seen = new Set<DeviceKey>();
  const walk = (i: CanvasItem, depth: number) => {
    const k = i.view.device.key;
    if (seen.has(k)) return;
    seen.add(k);
    out.push({ item: i, depth: Math.min(depth, 3) });
    for (const c of kids.get(k) ?? []) walk(c, depth + 1);
  };
  for (const r of roots) walk(r, 0);
  // A parent_hub cycle leaves items unvisited: list them flat.
  for (const i of items) walk(i, 0);
  return out;
}

/** Devices whose effective seats differ between the saved config and the draft ("Moved, not saved"). */
export function movedKeys(saved: Config | null, draft: Config | null, views: DeviceView[]): Set<DeviceKey> {
  const out = new Set<DeviceKey>();
  if (!saved || !draft || saved === draft) return out;
  const inv = views.map((v) => v.device);
  const a = effectiveMasks(saved, inv);
  const b = effectiveMasks(draft, inv);
  for (const d of inv) {
    if (a.get(d.key)?.mask !== b.get(d.key)?.mask) out.add(d.key);
  }
  return out;
}

export type MissingKind = 'display' | 'keyboard' | 'mouse' | 'audio';

export interface Missing {
  kind: MissingKind;
  /** The seat cannot start without it (screen, keyboard). */
  block: boolean;
}

function isPointer(d: Device): boolean {
  if (d.kind === 'mouse') return true;
  return d.kind === 'hid' && d.details.type === 'input' && d.details.usage_page === 0x0d;
}

/** One line per kind a seat lacks, from the plugged-in devices shown in its card. Only kinds
 *  the running backends assign: VM round 2 (2026-11-17), under fast user switching the display
 *  backend is `console_only`, the service lists no monitor, and every seat (seat 1 running
 *  too) read "cannot start without a screen". */
export function missingKinds(items: CanvasItem[], active?: Pick<ActiveBackends, 'display' | 'input'>): Missing[] {
  const present = items.map((i) => i.view.device).filter((d) => d.present);
  const out: Missing[] = [];
  const monitors = active?.display !== 'console_only';
  const inputs = active?.input !== 'none';
  if (monitors && !present.some((d) => d.kind === 'monitor')) out.push({ kind: 'display', block: true });
  if (inputs && !present.some((d) => d.kind === 'keyboard')) out.push({ kind: 'keyboard', block: true });
  if (inputs && !present.some(isPointer)) out.push({ kind: 'mouse', block: false });
  if (!present.some((d) => d.kind === 'audio_render')) out.push({ kind: 'audio', block: false });
  return out;
}

/** The short ID on a device's meta line: its location label (`USB-2-1-1`, `1.2`). */
export function shortId(d: Device): string | null {
  return d.label ?? null;
}

const KIND_ICONS: Record<DeviceKind, IconName> = {
  monitor: 'monitor',
  keyboard: 'keyboard',
  mouse: 'mouse',
  hid: 'touchpad',
  audio_render: 'speaker',
  audio_capture: 'mic',
  webcam: 'webcam',
  smart_card_reader: 'card',
  usb_hub: 'hub',
  storage: 'drive',
  printer: 'printer',
  serial_port: 'usb',
  network: 'wifi',
  other: 'usb'
};

/** A Lucide icon for a device kind (dialog row leads; the board itself draws DeviceIcon). */
export function kindIcon(kind: DeviceKind): IconName {
  return KIND_ICONS[kind] ?? 'usb';
}
