// SPDX-License-Identifier: GPL-3.0-only
// Device pictures (spec 10 §3.1, §7.9). Every built-in picture is our own neutral line
// drawing (DeviceIcon.svelte); administrators can add their own files in category folders
// under `%ProgramData%\Pulsar Seat\pictures\<category>\` (read by the Tauri side). The choice is stored
// per device in the config (`options.picture`): `builtin:<id>` or `user:<file name>`.
//
// "In use" / "not in use" pairs (spec 10 §3.1): a user file `name_off.ext` is the "not in
// use" picture of `name.ext`; when it is missing, the default picture is drawn instead.

import type { DeviceKind, UserPicture } from './types.ts';

export interface BuiltinPicture {
  id: string;
  kind: DeviceKind;
  label: string;
}

/** Built-in alternatives per category (the plain category picture is the default). */
export const BUILTIN_PICTURES: BuiltinPicture[] = [
  { id: 'monitor-speakers', kind: 'monitor', label: 'Hoparlörlü ekran' },
  { id: 'monitor-curved', kind: 'monitor', label: 'Kavisli ekran' },
  { id: 'tv', kind: 'monitor', label: 'Televizyon' },
  { id: 'laptop', kind: 'monitor', label: 'Dizüstü ekranı' },
  { id: 'keyboard-compact', kind: 'keyboard', label: 'Kısa klavye' },
  { id: 'keyboard-numpad', kind: 'keyboard', label: 'Sayı tuşlu klavye' },
  { id: 'keyboard-laptop', kind: 'keyboard', label: 'Dizüstü klavyesi' },
  { id: 'mouse-wireless', kind: 'mouse', label: 'Kablosuz fare' },
  { id: 'mouse-vertical', kind: 'mouse', label: 'Dikey fare' },
  { id: 'trackball', kind: 'mouse', label: 'İztopu' },
  { id: 'touchpad', kind: 'hid', label: 'Dokunmatik yüzey' },
  { id: 'trackpoint', kind: 'hid', label: 'İşaret çubuğu' },
  { id: 'pen', kind: 'hid', label: 'Kalem' },
  { id: 'touchscreen', kind: 'hid', label: 'Dokunmatik ekran' },
  { id: 'gamepad', kind: 'hid', label: 'Oyun kumandası' },
  { id: 'headphones', kind: 'audio_render', label: 'Kulaklık' },
  { id: 'speaker-single', kind: 'audio_render', label: 'Tek hoparlör' },
  { id: 'headset', kind: 'audio_capture', label: 'Mikrofonlu kulaklık' },
  { id: 'webcam-clip', kind: 'webcam', label: 'Ekran üstü kamera' }
];

/** User picture folder of a device kind (spec 10 §3.1), or null when the kind has none. */
export function categoryOf(kind: DeviceKind): string | null {
  switch (kind) {
    case 'monitor':
    case 'keyboard':
    case 'mouse':
    case 'hid':
    case 'audio_render':
    case 'audio_capture':
      return kind;
    case 'webcam':
      return 'video_capture';
    default:
      return null;
  }
}

export function builtinsFor(kind: DeviceKind): BuiltinPicture[] {
  return BUILTIN_PICTURES.filter((p) => p.kind === kind);
}

export type PictureRef = { type: 'default' } | { type: 'builtin'; id: string } | { type: 'user'; name: string };

export function parsePicture(v: string | undefined | null): PictureRef {
  if (!v) return { type: 'default' };
  if (v.startsWith('builtin:')) {
    const id = v.slice(8);
    return BUILTIN_PICTURES.some((p) => p.id === id) ? { type: 'builtin', id } : { type: 'default' };
  }
  if (v.startsWith('user:') && v.length > 5) return { type: 'user', name: v.slice(5) };
  return { type: 'default' };
}

export function pictureValue(r: PictureRef): string | undefined {
  if (r.type === 'builtin') return `builtin:${r.id}`;
  if (r.type === 'user') return `user:${r.name}`;
  return undefined;
}

/** `pen.png` → `pen_off.png`. */
export function offName(name: string): string {
  const dot = name.lastIndexOf('.');
  return dot > 0 ? `${name.slice(0, dot)}_off${name.slice(dot)}` : `${name}_off`;
}

/** User files that are the "not in use" half of a pair are not offered on their own. */
export function selectableUserPictures(list: UserPicture[]): UserPicture[] {
  const names = new Set(list.map((p) => p.name));
  return list.filter((p) => !(/_off\.[^.]+$/.test(p.name) && names.has(p.name.replace(/_off(\.[^.]+)$/, '$1'))));
}

/**
 * The data URL to draw for a user picture, or null = draw the default picture (file gone,
 * or "not in use" without its `_off` file).
 */
export function userPictureUrl(list: UserPicture[] | undefined, name: string, active: boolean): string | null {
  if (!list) return null;
  const want = active ? name : offName(name);
  return list.find((p) => p.name === want)?.data_url ?? null;
}
