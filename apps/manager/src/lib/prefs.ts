// SPDX-License-Identifier: GPL-3.0-only
// Per-user UI preferences (spec 10 §4.3–§4.5). They live in the app's own per-user storage
// (WebView localStorage), never machine-wide (architecture §6). Machine-wide behaviour
// (admin-only, start mode, ...) is in the service config instead.

import { ALL_SEATS_BITS } from './mask.ts';

export type Theme = 'system' | 'light' | 'dark';
/** The manager's views (rail places plus the Advanced sub-page of Settings). Older builds
 *  stored `basic` and `network`; `parsePrefs` maps them to `settings` and `advanced`. */
export type Tab = 'seats' | 'remote' | 'settings' | 'advanced' | 'status';
export const TABS: readonly Tab[] = ['seats', 'remote', 'settings', 'advanced', 'status'];

export interface Prefs {
  scaleIcons: boolean;
  evenRows: boolean;
  /** "Just connected" frame duration, 2..=20 s in steps of 2 (default 4). */
  flashSeconds: number;
  showAllHubs: boolean;
  sharedInSeats: boolean;
  showIds: boolean;
  visibleSeats: number;
  askOnManualStart: boolean;
  noShowGoodApply: boolean;
  hideTray: boolean;
  theme: Theme;
  tab: Tab;
  trayNoticeShown: boolean;
  /** "Don't show again" of the same-account dialog (spec 10 §6.2). */
  hideSameAccountWarning: boolean;
}

export const DEFAULT_PREFS: Prefs = {
  scaleIcons: true,
  evenRows: false,
  flashSeconds: 4,
  showAllHubs: true,
  sharedInSeats: false,
  showIds: false,
  visibleSeats: ALL_SEATS_BITS,
  askOnManualStart: true,
  noShowGoodApply: false,
  hideTray: false,
  theme: 'system',
  tab: 'seats',
  trayNoticeShown: false,
  hideSameAccountWarning: false
};

const KEY = 'pulsar-seat.manager.prefs.v1';

export function clampFlash(n: number): number {
  if (!Number.isFinite(n)) return DEFAULT_PREFS.flashSeconds;
  const even = Math.round(n / 2) * 2;
  return Math.min(20, Math.max(2, even));
}

/** Parse stored prefs; unknown or broken values fall back to the defaults. */
export function parsePrefs(raw: string | null): Prefs {
  if (!raw) return { ...DEFAULT_PREFS };
  let v: Record<string, unknown>;
  try {
    v = JSON.parse(raw);
  } catch {
    return { ...DEFAULT_PREFS };
  }
  if (!v || typeof v !== 'object') return { ...DEFAULT_PREFS };
  const out: Prefs = { ...DEFAULT_PREFS };
  for (const k of Object.keys(DEFAULT_PREFS) as (keyof Prefs)[]) {
    const def = DEFAULT_PREFS[k];
    if (typeof v[k] === typeof def) (out as unknown as Record<string, unknown>)[k] = v[k];
  }
  out.flashSeconds = clampFlash(out.flashSeconds);
  if (!['system', 'light', 'dark'].includes(out.theme)) out.theme = 'system';
  const legacy: Record<string, Tab> = { basic: 'settings', network: 'advanced' };
  const tab = out.tab as string;
  out.tab = legacy[tab] ?? (TABS.includes(tab as Tab) ? (tab as Tab) : 'seats');
  out.visibleSeats = out.visibleSeats >>> 0;
  return out;
}

export function loadPrefs(): Prefs {
  try {
    return parsePrefs(globalThis.localStorage?.getItem(KEY) ?? null);
  } catch {
    return { ...DEFAULT_PREFS };
  }
}

export function savePrefs(p: Prefs): void {
  try {
    globalThis.localStorage?.setItem(KEY, JSON.stringify(p));
  } catch {
    // Storage blocked: preferences last for this run only.
  }
}
