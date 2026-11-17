// SPDX-License-Identifier: GPL-3.0-only
// Seat avatars: account picture as a data URL (no network), else the first letter.

import { activeLang } from '../i18n.ts';
import type { UserPicture } from './types.ts';

const MIMES = ['image/png', 'image/jpeg', 'image/bmp', 'image/gif'];

/** `data:` URL for a picture the service returned; null = show the initial. */
export function avatarSrc(p: UserPicture | null | undefined): string | null {
  if (!p || !p.data || !MIMES.includes(p.mime)) return null;
  if (!/^[A-Za-z0-9+/]+={0,2}$/.test(p.data)) return null;
  return `data:${p.mime};base64,${p.data}`;
}

/** First letter (Unicode-aware, upper case of the UI language: Turkish i -> İ). */
export function initial(name: string): string {
  const c = Array.from(name.trim())[0] ?? '?';
  return c.toLocaleUpperCase(activeLang() === 'tr' ? 'tr-TR' : 'en-US');
}
