// SPDX-License-Identifier: GPL-3.0-only
// Staged save of the remote pages (redesign P6, BEHAVIOUR change; needs maintainer testing on
// a real remote seat). A remote page keeps a draft and reports it here; the docked save bar
// commits it and the app's unsaved-changes guard asks before the draft would be lost. Only
// the page on screen reports, so there is at most one remote stage at a time.

import { getContext, setContext } from 'svelte';

const DOCK = Symbol('remote-stage-dock');

/** The app shell docks the remote save bar itself (called by App); pages then only report. */
export function provideStageDock(): void {
  setContext(DOCK, true);
}

/** A shell docks the save bar of the remote stage (outside the shell, e.g. in a page test,
 *  the page renders its own bar). */
export function hasStageDock(): boolean {
  return getContext<boolean | undefined>(DOCK) === true;
}

export interface RemoteStage {
  /** Number of changed values (0 = nothing staged). */
  count: number;
  /** What changed and what saving does (the save bar's muted line). */
  detail?: string;
  /** Saving is not possible now (an invalid field): the reason. */
  invalid?: string;
  /** Commit the draft; `true` when everything was stored. */
  save: () => Promise<boolean>;
  /** Throw the draft away (back to the stored values). */
  discard: () => void;
}

export const remoteStage = $state<{ current: RemoteStage | null }>({ current: null });

/** Something is staged on the remote page on screen. */
export function remoteDirty(): boolean {
  return (remoteStage.current?.count ?? 0) > 0;
}

/** Save the staged remote draft (no-op `true` when nothing is staged). */
export async function saveRemoteStage(): Promise<boolean> {
  const s = remoteStage.current;
  if (!s || s.count === 0) return true;
  if (s.invalid) return false;
  return s.save();
}

export function discardRemoteStage(): void {
  remoteStage.current?.discard();
}

/**
 * Number of leaf values that differ between two JSON-like values. Arrays count as one value
 * each (a changed list is one change), objects recurse.
 */
export function countChanges(a: unknown, b: unknown): number {
  if (Array.isArray(a) || Array.isArray(b)) return JSON.stringify(a ?? null) === JSON.stringify(b ?? null) ? 0 : 1;
  const ao = a !== null && typeof a === 'object';
  const bo = b !== null && typeof b === 'object';
  if (ao && bo) {
    const x = a as Record<string, unknown>;
    const y = b as Record<string, unknown>;
    let n = 0;
    for (const k of new Set([...Object.keys(x), ...Object.keys(y)])) n += countChanges(x[k], y[k]);
    return n;
  }
  if (ao || bo) return JSON.stringify(a ?? null) === JSON.stringify(b ?? null) ? 0 : 1;
  // undefined and null are the same "unset" value.
  return (a ?? null) === (b ?? null) ? 0 : 1;
}
