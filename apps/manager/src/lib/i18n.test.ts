// SPDX-License-Identifier: GPL-3.0-only
import { afterEach, describe, expect, it } from 'vitest';
import {
  activeLang,
  applyLocale,
  availabilityLabel,
  checkText,
  errorLabel,
  fallbackNoteLabel,
  initLocale,
  issueText,
  langFromTags,
  loopbackNoteLabel,
  noteLabel,
  parseLangPref,
  productLabel,
  resolveLang,
  rt,
  seatLabel,
  seatList,
  storeLangPref,
  storedLangPref,
  t
} from './i18n.ts';
import { en } from './locales/en.ts';
import { tr } from './locales/tr.ts';
import { accountEntryValid, loopbackPortValid, parseAccountList } from './model.ts';

afterEach(() => {
  storeLangPref('system');
  applyLocale('en');
});

describe('i18n (English default)', () => {
  it('labels states', () => {
    expect(activeLang()).toBe('en');
    expect(productLabel({ state: 'enabled', started: false })).toBe('On, seats not started');
    expect(seatLabel({ state: 'signed_in', user: 'ayse' })).toBe('Signed in: ayse');
    expect(seatLabel({ state: 'error', reason: 'logon_failed', detail: '' })).toContain('password');
  });

  it('maps errors by reason first, then code', () => {
    expect(errorLabel({ code: 'timeout', message: '', reason: 'session_timeout' })).toBe('The session did not start in time');
    expect(errorLabel({ code: 'access_denied', message: '' })).toContain('administrator');
    expect(errorLabel({ code: 'weird', message: '' })).toBe('Error: weird');
  });

  it('renders fallback notes as text, never as raw codes', () => {
    const a = fallbackNoteLabel('session:rds_loopback->fast_user_switch:edition_unsupported');
    expect(a).toContain('Session method');
    expect(a).toContain('fast user switching');
    expect(a).toContain('Not supported by this Windows edition');
    expect(a).not.toContain('->');
    expect(fallbackNoteLabel('gate:session_drive_letters:not_built')).toContain('Not in this build');
    expect(fallbackNoteLabel('display:console_only:fast_user_switch')).toContain('Console only');
    expect(fallbackNoteLabel('session:termsrv_wrapper:needs_setup_enable_termsrv_wrapper')).toContain('wrapper');
    expect(fallbackNoteLabel('input_not_isolated')).toBe('Input is not fully isolated');
  });

  it('shows check results and setup needs by id, not by the service detail', () => {
    expect(checkText('termservice_stock', 'pass')).toContain('Windows');
    expect(checkText('unknown_check', 'warn')).toBe('Warning');
    expect(availabilityLabel({ state: 'needs_setup', what: 'console_sign_in' })).toContain('sign in');
    expect(availabilityLabel({ state: 'needs_setup', what: 'some english text' })).toBe('Setup needed');
  });

  it('joins seat lists', () => {
    expect(seatList([2, 3, 4])).toBe('2, 3 and 4');
    expect(seatList([5])).toBe('5');
  });
});

describe('passwordless-seat listener copy', () => {
  it('names the listener state behind a seat failure', () => {
    const s = seatLabel({
      state: 'error',
      reason: 'loopback_listener_unavailable',
      detail: 'step=connecting reason=loopback_listener_unavailable listener=loopback_listener:unavailable:firewall_off'
    });
    expect(s).toContain('Windows Firewall');
    expect(seatLabel({ state: 'error', reason: 'loopback_listener_unavailable', detail: 'listener=loopback_listener:pending_restart:33389' })).toContain(
      'waiting for Remote Desktop Services to restart'
    );
    expect(seatLabel({ state: 'error', reason: 'nla_required', detail: '' })).toContain('Advanced');
  });

  it('labels probe notes with a port', () => {
    expect(noteLabel('loopback_listener:ready:33389')).toContain('33389');
    expect(loopbackNoteLabel('loopback_listener_exposed:33389')).toContain('WARNING');
    expect(noteLabel('loopback_listener:unavailable:restart_needs_reboot')).toContain('restart the PC');
    expect(loopbackNoteLabel('other')).toBeNull();
  });

  it('labels backend probe notes', () => {
    expect(noteLabel('kiosk_shell_seat_aware_apps_only')).toContain('kiosk');
    expect(noteLabel('two_admin_sessions_only')).toContain('two administrator');
    expect(noteLabel('loopback_listener_created')).toContain('created');
  });

  it('says what each session mode really is (spec 35 §6.2)', () => {
    expect(noteLabel('fus_inactive_frozen')).toContain('frozen');
    expect(noteLabel('fus_audio_per_user')).toContain('sound device');
    expect(noteLabel('kiosk_audio_shared')).toContain('seat 1’s');
    expect(t.backendNames.rds_loopback).toContain('Concurrent full sessions');
    expect(t.backendNames.fast_user_switch).toContain('one at a time');
    expect(t.backendNames.desktops).toContain('Concurrent kiosk seats');
    expect(issueText.hide_fsu_with_fast_user_switch).toContain('one seat at a time');
  });

  it('validates port and accounts', () => {
    expect(loopbackPortValid(33389)).toBe(true);
    for (const p of [80, 3389, 70000, 1.5]) expect(loopbackPortValid(p)).toBe(false);
    expect(parseAccountList(' ayse\nEV\\mehmet, AYSE\n\n')).toEqual(['ayse', 'EV\\mehmet']);
    expect(accountEntryValid('EV\\mehmet')).toBe(true);
    for (const a of ['', 'x@ev.local', 'a\\b\\c', '\\x']) expect(accountEntryValid(a)).toBe(false);
  });
});

/** Every key path of a locale object; functions and arrays are leaves (with their arity / length). */
function keyPaths(v: unknown, prefix = ''): string[] {
  if (typeof v === 'function') return [`${prefix}()${(v as (...a: unknown[]) => unknown).length}`];
  if (Array.isArray(v)) return [`${prefix}[${v.length}]`];
  if (v && typeof v === 'object') return Object.keys(v).flatMap((k) => keyPaths((v as Record<string, unknown>)[k], prefix ? `${prefix}.${k}` : k));
  return [prefix];
}

/** Every string leaf (functions called with sample arguments). */
function strings(v: unknown, prefix = ''): [string, string][] {
  if (typeof v === 'string') return [[prefix, v]];
  if (typeof v === 'function') {
    const f = v as (...a: unknown[]) => unknown;
    // Sample arguments: numbers first, then strings, then arrays (whatever the signature takes).
    for (const sample of [2, 'x', [2, 3]]) {
      try {
        const out = f(...Array(f.length).fill(sample));
        if (typeof out === 'string') return [[prefix, out]];
      } catch {
        // wrong sample type for this function: try the next one
      }
    }
    return [];
  }
  if (v && typeof v === 'object') return Object.keys(v).flatMap((k) => strings((v as Record<string, unknown>)[k], `${prefix}.${k}`));
  return [];
}

describe('locales', () => {
  it('English and Turkish have exactly the same keys', () => {
    const a = keyPaths(en).sort();
    const b = keyPaths(tr).sort();
    expect(b.filter((k) => !a.includes(k))).toEqual([]);
    expect(a.filter((k) => !b.includes(k))).toEqual([]);
  });

  it('no string is empty and the English table holds no Turkish letters', () => {
    // `none` is the dash placeholder; `printersShort.off` is empty on purpose.
    const allowedEmpty = (k: string) => k.endsWith('.printersShort.off');
    for (const [k, s] of strings(en)) {
      if (!allowedEmpty(k)) expect(s.trim(), k).not.toBe('');
      // Language names show themselves in their own language.
      if (!k.includes('languageNames')) expect(/[çğıöşüÇĞİÖŞÜ]/.test(s), `${k}: ${s}`).toBe(false);
    }
    for (const [k, s] of strings(tr)) if (!allowedEmpty(k)) expect(s.trim(), k).not.toBe('');
  });

  it('picks the language from tags and the stored choice', () => {
    expect(langFromTags(['tr-TR', 'en-US'])).toBe('tr');
    expect(langFromTags(['de-DE', 'en-GB'])).toBe('en');
    expect(langFromTags(['de-DE'])).toBe('en');
    expect(langFromTags([])).toBe('en');
    expect(langFromTags([null, undefined, 'TR'])).toBe('tr');
    expect(parseLangPref('tr')).toBe('tr');
    expect(parseLangPref('xx')).toBe('system');
    expect(resolveLang('system', ['tr-TR'])).toBe('tr');
    expect(resolveLang('en', ['tr-TR'])).toBe('en');
  });

  it('follows the OS language unless the user chose one', () => {
    expect(initLocale(['tr-TR'])).toBe('tr');
    expect(t.seats).toBe('Koltuklar');
    storeLangPref('en');
    expect(storedLangPref()).toBe('en');
    expect(initLocale(['tr-TR'])).toBe('en');
    expect(t.seats).toBe('Seats');
    storeLangPref('system');
    expect(storedLangPref()).toBe('system');
  });
});

describe('Turkish locale', () => {
  it('switches every table', () => {
    applyLocale('tr');
    expect(activeLang()).toBe('tr');
    expect(productLabel({ state: 'enabled', started: false })).toBe('Açık, koltuklar başlatılmadı');
    expect(seatLabel({ state: 'signed_in', user: 'ayse' })).toBe('Oturum açık: ayse');
    expect(errorLabel({ code: 'timeout', message: '', reason: 'session_timeout' })).toBe('Oturum zamanında başlamadı');
    expect(errorLabel({ code: 'weird', message: '' })).toBe('Hata: weird');
    expect(fallbackNoteLabel('session:rds_loopback->fast_user_switch:edition_unsupported')).toContain('hızlı kullanıcı değiştirme');
    expect(loopbackNoteLabel('loopback_listener_exposed:33389')).toContain('DİKKAT');
    expect(checkText('unknown_check', 'warn')).toBe('Uyarı');
    expect(seatList([2, 3, 4])).toBe('2, 3 ve 4');
    expect(rt.failure.wrong_pin).toBe('PIN yanlış.');
    expect(t.basic.languageNames.tr).toBe('Türkçe');
    applyLocale('en');
    expect(t.seats).toBe('Seats');
    expect(rt.failure.wrong_pin).toBe('Wrong PIN.');
  });
});
