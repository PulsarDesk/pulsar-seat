// SPDX-License-Identifier: GPL-3.0-only
// Seat avatars (spec/remote 10 §2.1, §4.2): the seat user's Windows account picture as a
// data URL (no network), else the seat's first letter. Restored by the review of 2026-11-17
// after redesign P6 had dropped it.

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

/**
 * Users whose picture is still to be asked for: every seat user not in `known` (a user
 * name change asks again, because the new name is not known yet).
 */
export function picturesToLoad(users: string[], known: Record<string, unknown>): string[] {
  return [...new Set(users.filter((u) => u && !(u in known)))];
}
