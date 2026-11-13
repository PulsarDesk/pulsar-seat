// SPDX-License-Identifier: GPL-3.0-only
// Client-side mirror of `split-core::validate` so the UI can react while typing. The
// service validates again and is authoritative.

export const PORT_MIN = 1029;
export const PORT_MAX = 65514;
export const NAME_MAX = 128;

export type FieldError =
  | 'name_empty'
  | 'name_chars'
  | 'name_long'
  | 'name_duplicate'
  | 'display_name'
  | 'display_name_duplicate'
  | 'port'
  | 'port_overlap';

export function seatNameError(name: string, existing: string[] = []): FieldError | null {
  if (name.length === 0) return 'name_empty';
  if (name.length > NAME_MAX) return 'name_long';
  if (!/^[A-Za-z0-9]+$/.test(name)) return 'name_chars';
  const lower = name.toLowerCase();
  if (existing.some((e) => e.toLowerCase() === lower)) return 'name_duplicate';
  return null;
}

/** `existing`: display names of the other seats (unique, case-insensitive: spec 80 §7.3). */
export function displayNameError(name: string, existing: string[] = []): FieldError | null {
  // eslint-disable-next-line no-control-regex
  if (name.length === 0 || name.trim() !== name || /[\u0000-\u001f\u007f]/.test(name)) return 'display_name';
  const lower = name.toLowerCase();
  if (existing.some((e) => e.toLowerCase() === lower)) return 'display_name_duplicate';
  return null;
}

export function portError(text: string): FieldError | null {
  if (!/^\d{1,5}$/.test(text.trim())) return 'port';
  const p = Number(text.trim());
  return p >= PORT_MIN && p <= PORT_MAX ? null : 'port';
}

/** A seat with base `p` reserves `p-5 ..= p+21`; two blocks overlap when bases are < 27 apart. */
export function blocksOverlap(a: number, b: number): boolean {
  return Math.abs(a - b) <= 26;
}

export function portOverlapError(port: number, otherPorts: number[]): FieldError | null {
  return otherPorts.some((o) => blocksOverlap(port, o)) ? 'port_overlap' : null;
}
