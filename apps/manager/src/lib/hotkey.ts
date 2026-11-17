// SPDX-License-Identifier: GPL-3.0-only
// Keyboard/mouse switch hotkey (spec 10 §7.6, spec 50 §4.3): Ctrl + one key, stored as a
// PC/AT set-1 make code (`settings.input.switch_hotkey_scan`). The dialog reads
// `KeyboardEvent.code` (layout independent, i.e. the physical key) and maps it here.

import { t } from './i18n.ts';

const CODE_TO_SCAN: Record<string, number> = {
  Escape: 0x01,
  Digit1: 0x02,
  Digit2: 0x03,
  Digit3: 0x04,
  Digit4: 0x05,
  Digit5: 0x06,
  Digit6: 0x07,
  Digit7: 0x08,
  Digit8: 0x09,
  Digit9: 0x0a,
  Digit0: 0x0b,
  Minus: 0x0c,
  Equal: 0x0d,
  Backspace: 0x0e,
  Tab: 0x0f,
  KeyQ: 0x10,
  KeyW: 0x11,
  KeyE: 0x12,
  KeyR: 0x13,
  KeyT: 0x14,
  KeyY: 0x15,
  KeyU: 0x16,
  KeyI: 0x17,
  KeyO: 0x18,
  KeyP: 0x19,
  BracketLeft: 0x1a,
  BracketRight: 0x1b,
  Enter: 0x1c,
  KeyA: 0x1e,
  KeyS: 0x1f,
  KeyD: 0x20,
  KeyF: 0x21,
  KeyG: 0x22,
  KeyH: 0x23,
  KeyJ: 0x24,
  KeyK: 0x25,
  KeyL: 0x26,
  Semicolon: 0x27,
  Quote: 0x28,
  Backquote: 0x29,
  Backslash: 0x2b,
  KeyZ: 0x2c,
  KeyX: 0x2d,
  KeyC: 0x2e,
  KeyV: 0x2f,
  KeyB: 0x30,
  KeyN: 0x31,
  KeyM: 0x32,
  Comma: 0x33,
  Period: 0x34,
  Slash: 0x35,
  NumpadMultiply: 0x37,
  Space: 0x39,
  F1: 0x3b,
  F2: 0x3c,
  F3: 0x3d,
  F4: 0x3e,
  F5: 0x3f,
  F6: 0x40,
  F7: 0x41,
  F8: 0x42,
  F9: 0x43,
  F10: 0x44,
  Numpad7: 0x47,
  Numpad8: 0x48,
  Numpad9: 0x49,
  NumpadSubtract: 0x4a,
  Numpad4: 0x4b,
  Numpad5: 0x4c,
  Numpad6: 0x4d,
  NumpadAdd: 0x4e,
  Numpad1: 0x4f,
  Numpad2: 0x50,
  Numpad3: 0x51,
  Numpad0: 0x52,
  NumpadDecimal: 0x53,
  IntlBackslash: 0x56,
  F11: 0x57,
  F12: 0x58
};

const SCAN_TO_CODE: Record<number, string> = Object.fromEntries(Object.entries(CODE_TO_SCAN).map(([c, s]) => [s, c]));

/** Modifier keys and locks cannot be the hotkey key. */
const REJECTED = new Set(['ControlLeft', 'ControlRight', 'ShiftLeft', 'ShiftRight', 'AltLeft', 'AltRight', 'MetaLeft', 'MetaRight', 'CapsLock', 'NumLock', 'ScrollLock']);

export const DEFAULT_HOTKEY_SCAN = 0x58;

export function scanFromCode(code: string): number | null {
  if (REJECTED.has(code)) return null;
  return CODE_TO_SCAN[code] ?? null;
}

const SPECIAL_NAMES: Record<string, string> = {
  Escape: 'Esc',
  Enter: 'Enter',
  Minus: '-',
  Equal: '=',
  BracketLeft: '[',
  BracketRight: ']',
  Semicolon: ';',
  Quote: "'",
  Backquote: '`',
  Backslash: '\\',
  Comma: ',',
  Period: '.',
  Slash: '/',
  IntlBackslash: '<',
  NumpadMultiply: 'Num *',
  NumpadSubtract: 'Num -',
  NumpadAdd: 'Num +',
  NumpadDecimal: 'Num ,'
};

/** Human-readable name of a key code (physical US-layout position). */
export function codeName(code: string): string {
  if (t.keyNames[code]) return t.keyNames[code];
  if (SPECIAL_NAMES[code]) return SPECIAL_NAMES[code];
  if (code.startsWith('Key')) return code.slice(3);
  if (code.startsWith('Digit')) return code.slice(5);
  if (code.startsWith('Numpad')) return `Num ${code.slice(6)}`;
  return code;
}

/** `Ctrl + F12`, or null when the scan code is unknown. */
export function hotkeyLabel(scan: number | null): string | null {
  if (scan === null) return null;
  const code = SCAN_TO_CODE[scan];
  return code ? `Ctrl + ${codeName(code)}` : `Ctrl + 0x${scan.toString(16).padStart(2, '0')}`;
}
