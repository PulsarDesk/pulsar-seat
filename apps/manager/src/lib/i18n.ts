// SPDX-License-Identifier: GPL-3.0-only
// UI copy access. The strings live in `locales/en.ts` (default) and `locales/tr.ts` (same
// keys). The exported tables (`t`, `rt`, `failureText`, ...) are filled from the active
// locale once at start-up (`applyLocale`); the language follows the Windows display
// language unless the user picked one in the Basic settings tab (per-user, localStorage).
// Changing it reloads the window, so components never see a half-switched table.

import type { Availability, CheckOutcome, DeviceKind, DriverStatus, Issue, ProductState, ProtoError, SeatState } from './types.ts';
import type { ErrorCode as RemoteErrorCode, ProtoError as RemoteProtoError } from './remote/types.ts';
import type { FieldError as RemoteFieldError } from './remote/validation.ts';
import { en, type Locale } from './locales/en.ts';
import { tr } from './locales/tr.ts';

export type { Locale };

/** Languages the manager ships. English is the default and the fallback. */
export type LangCode = 'en' | 'tr';
export const LANGS: readonly LangCode[] = ['en', 'tr'];
/** The user's choice: a language, or follow Windows. */
export type LangPref = LangCode | 'system';

export const LOCALES: Record<LangCode, Locale> = { en, tr };
const LANG_KEY = 'pulsar-seat.manager.lang.v1';

/** First supported language among BCP 47 tags (`tr-TR`, `en-GB`, ...); English otherwise. */
export function langFromTags(tags: readonly (string | null | undefined)[]): LangCode {
  for (const tag of tags) {
    const primary = (tag ?? '').trim().toLowerCase().split(/[-_]/)[0];
    if ((LANGS as readonly string[]).includes(primary)) return primary as LangCode;
  }
  return 'en';
}

/** The WebView's language list; WebView2 takes it from the Windows display language. */
export function browserLangTags(): string[] {
  const nav = (globalThis as { navigator?: { languages?: readonly string[]; language?: string } }).navigator;
  if (!nav) return [];
  if (nav.languages && nav.languages.length) return [...nav.languages];
  return nav.language ? [nav.language] : [];
}

export function parseLangPref(raw: string | null | undefined): LangPref {
  return raw === 'en' || raw === 'tr' ? raw : 'system';
}

export function storedLangPref(): LangPref {
  try {
    return parseLangPref(globalThis.localStorage?.getItem(LANG_KEY));
  } catch {
    return 'system';
  }
}

export function storeLangPref(pref: LangPref): void {
  try {
    if (pref === 'system') globalThis.localStorage?.removeItem(LANG_KEY);
    else globalThis.localStorage?.setItem(LANG_KEY, pref);
  } catch {
    // Storage blocked: the choice lasts for this window only.
  }
}

/** Pick the language: the user's choice wins, then the OS language tags. */
export function resolveLang(pref: LangPref, osTags: readonly (string | null | undefined)[]): LangCode {
  return pref === 'system' ? langFromTags(osTags) : pref;
}

type Tables = Omit<Locale, 'misc'>;
// The exported tables are stable objects; `applyLocale` swaps their contents.
export const t = {} as Locale['t'];
export const rt = {} as Locale['rt'];
export const safeModeText: Record<string, string> = {};
export const failureText: Record<string, string> = {};
export const issueText: Record<string, string> = {};
export const noticeText: Record<string, string> = {};
export const loopbackStateText: Record<string, string> = {};
export const remoteFieldErrorText = {} as Record<RemoteFieldError, string>;
/** Update dialog copy (`t.update`), re-exported by update.ts. */
export const updateText = {} as Locale['t']['update'];
const productText: Record<string, string> = {};
const errorText: Record<string, string> = {};
const remoteErrorText = {} as Record<RemoteErrorCode, string>;
let misc = en.misc;
let current: LangCode = 'en';

function refill<T extends object>(target: T, source: T): void {
  for (const k of Object.keys(target)) delete (target as Record<string, unknown>)[k];
  Object.assign(target, source);
}

/** Switch every exported table to `lang`. Components read the tables when they render. */
export function applyLocale(lang: LangCode): void {
  const l: Tables & Pick<Locale, 'misc'> = LOCALES[lang];
  refill(t, l.t);
  refill(rt, l.rt);
  refill(safeModeText, l.safeModeText);
  refill(failureText, l.failureText);
  refill(issueText, l.issueText);
  refill(noticeText, l.noticeText);
  refill(loopbackStateText, l.loopbackStateText);
  refill(remoteFieldErrorText, l.remoteFieldErrorText);
  refill(productText, l.productText);
  refill(errorText, l.errorText);
  refill(remoteErrorText, l.remoteErrorText);
  refill(updateText, l.t.update);
  misc = l.misc;
  current = lang;
  const doc = (globalThis as { document?: { documentElement?: { lang: string } } }).document;
  if (doc?.documentElement) doc.documentElement.lang = lang;
}

/** The language in use now. */
export function activeLang(): LangCode {
  return current;
}

/**
 * Start-up: apply the stored choice or the OS language. `osTags` comes from the manager
 * backend (Windows user UI language); the WebView's own list is the fallback.
 */
export function initLocale(osTags?: readonly (string | null | undefined)[]): LangCode {
  const tags = osTags && osTags.some(Boolean) ? osTags : browserLangTags();
  const lang = resolveLang(storedLangPref(), tags);
  applyLocale(lang);
  return lang;
}

initLocale();

export function productLabel(p: ProductState): string {
  if (p.state === 'enabled') return p.started ? productText.enabledStarted : productText.enabledNotStarted;
  return productText[p.state] ?? p.state;
}

export function seatLabel(s: SeatState): string {
  const base = t.seatState[s.state] ?? s.state;
  if (s.state === 'signed_in' || s.state === 'locked') return `${base}: ${s.user}`;
  if (s.state === 'starting') return `${base} (${t.phases[s.phase] ?? s.phase})`;
  if (s.state === 'error') {
    const why = failureText[s.reason] ?? s.reason;
    const more = s.reason === 'loopback_listener_unavailable' ? loopbackDetailText(s.detail) : null;
    return more ? `${base} (${why}: ${more})` : `${base} (${why})`;
  }
  return base;
}

/** Error code scheme shown to the user: subsystem/code plus the Windows code. */
export function errorCodeTag(e: ProtoError): string {
  const parts = [e.reason ?? e.code];
  if (e.win32 !== undefined && e.win32 !== null) parts.push(`Windows ${e.win32}`);
  return parts.join(' · ');
}

export function errorLabel(e: unknown): string {
  const pe = e as ProtoError;
  if (typeof e === 'string') return misc.errorPrefix(e);
  const key = pe?.reason && failureText[pe.reason] ? pe.reason : pe?.code;
  return failureText[key ?? ''] ?? errorText[key ?? ''] ?? misc.errorPrefix(pe?.code ?? String(e));
}

export function issueLabel(i: Issue, deviceName?: (key: string) => string): string {
  const base = issueText[i.code] ?? i.code;
  const where: string[] = [];
  if (i.seat) where.push(misc.issueSeat(i.seat));
  if (i.device) where.push(deviceName ? deviceName(i.device) : i.device);
  return where.length ? `${where.join(', ')}: ${base}` : base;
}

export function noticeLabel(code: string): string {
  return noticeText[code] ?? code;
}

export function availabilityLabel(a: Availability): string {
  const base = t.availability[a.state] ?? a.state;
  if (a.state === 'unavailable') return `${base}: ${t.unavailableReasons[a.reason] ?? a.reason}`;
  if (a.state === 'needs_setup') {
    const what = t.needsSetup[a.what];
    return what ? `${base}: ${what}` : base;
  }
  if (a.state === 'driver_not_signed' || a.state === 'driver_version_mismatch') {
    return `${base} (${t.drivers[a.driver] ?? a.driver})`;
  }
  return base;
}

export function driverStatusLabel(s: DriverStatus): string {
  const base = t.driverStatus[s.state] ?? s.state;
  if (s.state === 'loaded') return `${base} (${s.interface_major}.${s.interface_minor}, ${s.build})`;
  if (s.state === 'version_mismatch') return `${base} (${s.found})`;
  return base;
}

/** Text of one refused redirection request. */
export function refusalLabel(r: { channel: string; reason: string; suppressed?: number }): string {
  const ch = t.redirect.channels[r.channel] ?? r.channel;
  const why = t.refusalReasons[r.reason] ?? r.reason;
  return `${ch}: ${why}${r.suppressed ? ` (+${r.suppressed})` : ''}`;
}

export function noteLabel(code: string): string {
  return t.notes[code] ?? loopbackNoteLabel(code) ?? code;
}

/** `listener=loopback_listener:<state>[:<x>]` inside a seat failure `detail` → text, or null. */
export function loopbackDetailText(detail: string): string | null {
  const m = /listener=loopback_listener:([a-z_]+)(?::([a-z_0-9]+))?/.exec(detail);
  if (!m) return null;
  const key = m[1] === 'unavailable' && m[2] ? m[2] : m[1];
  return loopbackStateText[key] ?? key;
}

/** Probe notes of the passwordless-seat listener (with a port or reason suffix). */
export function loopbackNoteLabel(code: string): string | null {
  const port = code.slice(code.lastIndexOf(':') + 1);
  if (code.startsWith('loopback_listener_exposed:')) {
    return misc.loopbackExposed(port);
  }
  if (code.startsWith('loopback_listener_unload_pending:')) {
    return misc.loopbackUnloadPending(port);
  }
  if (!code.startsWith('loopback_listener:')) return null;
  const [, state, x] = code.split(':');
  const head = misc.loopbackHead;
  if (state === 'ready') return `${head}: ${misc.loopbackReady(x)}`;
  const key = state === 'unavailable' && x ? x : state;
  return `${head}: ${loopbackStateText[key] ?? key}`;
}

export function backendName(kind: string): string {
  return t.backendNames[kind] ?? kind;
}

export function checkLabel(id: string): string {
  return t.checks[id] ?? id;
}

/** Text for one check result (by id + outcome); falls back to the outcome word. */
export function checkText(id: string, outcome: CheckOutcome): string {
  return t.checkText[id]?.[outcome] ?? t.outcomes[outcome];
}

/** Text for a backend reason code (`unavailable`, `needs_setup_*`, `driver_*_<id>`). */
export function reasonLabel(code: string): string {
  const direct = t.unavailableReasons[code];
  if (direct) return direct;
  if (code.startsWith('needs_setup_')) {
    const what = t.needsSetup[code.slice('needs_setup_'.length)];
    return what ? `${t.availability.needs_setup}: ${what}` : t.availability.needs_setup;
  }
  for (const state of ['driver_not_signed', 'driver_version_mismatch'] as const) {
    if (code.startsWith(`${state}_`)) {
      const drv = code.slice(state.length + 1);
      return `${t.availability[state]} (${t.drivers[drv] ?? drv})`;
    }
  }
  return t.fallback.unknownReason(code);
}

const fallbackCategory: Record<string, () => string> = {
  session: () => t.advanced.session,
  display: () => t.advanced.display,
  input: () => t.advanced.input,
  audio: () => t.advanced.audio,
  gate: () => t.fallback.gate
};

/**
 * Text for one `ServiceStatus.fallback_notes` entry. Shapes from the service:
 * `cat:wanted->used:reason`, `cat:wanted:reason`, or a plain note code.
 */
export function fallbackNoteLabel(note: string): string {
  const first = note.indexOf(':');
  if (first < 0) return noteLabel(note);
  const cat = note.slice(0, first);
  const rest = note.slice(first + 1);
  const second = rest.indexOf(':');
  const catText = fallbackCategory[cat]?.() ?? cat;
  if (second < 0) return `${catText}: ${backendName(rest)}`;
  const kinds = rest.slice(0, second);
  const reason = rest.slice(second + 1);
  const arrow = kinds.indexOf('->');
  if (arrow >= 0) {
    const wanted = backendName(kinds.slice(0, arrow));
    const used = backendName(kinds.slice(arrow + 2));
    return `${catText}: ${t.fallback.instead(wanted, used)} — ${reasonLabel(reason)}`;
  }
  if (reason === 'fast_user_switch') return `${catText}: ${t.fallback.forcedByFus(backendName(kinds))}`;
  return `${catText}: ${t.fallback.notUsable(backendName(kinds))} — ${reasonLabel(reason)}`;
}

/** Seat list for messages: "2, 3 and 4". */
export function seatList(seats: number[]): string {
  if (seats.length <= 1) return seats.map(String).join('');
  return `${seats.slice(0, -1).join(', ')}${misc.listAnd}${seats[seats.length - 1]}`;
}

export function kindLabel(k: DeviceKind): string {
  return t.kinds[k] ?? k;
}

/** Processor architecture for people: `aarch64` → ARM64, `x86_64` → x64 (raw codes never reach the screen). */
export function archLabel(arch: string | null | undefined): string {
  if (!arch) return t.none;
  return t.archNames[arch] ?? arch;
}

/** Monitor connector kind (`DisplayInfo.output_technology`): `internal` → built-in screen, `hdmi` → HDMI. */
export function outputTechLabel(kind: string | null | undefined): string {
  if (!kind) return t.outputTech.other;
  return t.outputTech[kind] ?? t.outputTech.other;
}

/** Text for a package reason code; unknown codes fall back to a generic text. */
export function remoteFailureText(reason: string | null | undefined): string {
  if (!reason) return rt.failure.unsupported;
  return rt.failure[reason] ?? remoteErrorText[reason as RemoteErrorCode] ?? `${remoteErrorText.internal} (${reason})`;
}

export function isRemoteProtoError(e: unknown): e is RemoteProtoError {
  return typeof e === 'object' && e !== null && 'code' in e && typeof (e as RemoteProtoError).code === 'string';
}

/** Text for any error thrown by the API layer. */
export function remoteErrorMessage(e: unknown): string {
  if (!isRemoteProtoError(e)) return remoteErrorText.internal;
  if (e.reason && rt.failure[e.reason]) return rt.failure[e.reason];
  if (e.code === 'duplicate_name' && e.field === 'display_name') return remoteFieldErrorText.display_name_duplicate;
  return remoteErrorText[e.code] ?? remoteErrorText.internal;
}
