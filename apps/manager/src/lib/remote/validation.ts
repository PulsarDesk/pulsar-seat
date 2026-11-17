// SPDX-License-Identifier: GPL-3.0-only
// Client-side mirror of `seat_core::remote::validate` (was `split-core::validate`) so the UI can react while typing. The
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

/** UDP port of the Pulsar LAN beacon (`seat_core::remote::validate::PULSAR_DISCOVERY_PORT`). */
export const PULSAR_DISCOVERY_PORT = 21117;

/**
 * A fixed Pulsar node port that the service refuses (mirror of `seat_against`): inside
 * another seat's port block (`base-5 ..= base+21`), equal to another seat's fixed node
 * port or forwarded UDP host port (`otherNodePorts`), or the LAN discovery port.
 */
export function nodePortConflict(port: number, otherPorts: number[], otherNodePorts: number[] = []): boolean {
  if (port === PULSAR_DISCOVERY_PORT) return true;
  if (otherPorts.some((b) => b > 0 && port >= b - 5 && port <= b + 21)) return true;
  return otherNodePorts.includes(port);
}
