// SPDX-License-Identifier: GPL-3.0-only
import { describe, expect, it } from 'vitest';
import { availabilityLabel, checkText, errorLabel, fallbackNoteLabel, loopbackNoteLabel, noteLabel, productLabel, seatLabel, issueText, t } from './i18n.ts';
import { accountEntryValid, loopbackPortValid, parseAccountList } from './model.ts';

describe('i18n', () => {
  it('labels states in Turkish', () => {
    expect(productLabel({ state: 'enabled', started: false })).toBe('Açık, koltuklar başlatılmadı');
    expect(seatLabel({ state: 'signed_in', user: 'ayse' })).toBe('Oturum açık: ayse');
    expect(seatLabel({ state: 'error', reason: 'logon_failed', detail: '' })).toContain('parola');
  });

  it('maps errors by reason first, then code', () => {
    expect(errorLabel({ code: 'timeout', message: '', reason: 'session_timeout' })).toBe('Oturum zamanında başlamadı');
    expect(errorLabel({ code: 'access_denied', message: '' })).toContain('yönetici');
    expect(errorLabel({ code: 'weird', message: '' })).toBe('Hata: weird');
  });

  it('renders fallback notes in Turkish, never as raw codes', () => {
    const a = fallbackNoteLabel('session:rds_loopback->fast_user_switch:edition_unsupported');
    expect(a).toContain('Oturum yöntemi');
    expect(a).toContain('hızlı kullanıcı değiştirme');
    expect(a).toContain('Bu Windows sürümü desteklemiyor');
    expect(a).not.toContain('->');
    expect(fallbackNoteLabel('gate:session_drive_letters:not_built')).toContain('Bu derlemede yok');
    expect(fallbackNoteLabel('display:console_only:fast_user_switch')).toContain('Yalnızca konsol');
    expect(fallbackNoteLabel('session:termsrv_wrapper:needs_setup_enable_termsrv_wrapper')).toContain('sarmalayıcı');
    expect(fallbackNoteLabel('input_not_isolated')).toBe('Giriş tam yalıtılmıyor');
  });

  it('shows check results and setup needs by id, not by English detail', () => {
    expect(checkText('termservice_stock', 'pass')).toContain('Windows');
    expect(checkText('unknown_check', 'warn')).toBe('Uyarı');
    expect(availabilityLabel({ state: 'needs_setup', what: 'console_sign_in' })).toContain('oturum aç');
    expect(availabilityLabel({ state: 'needs_setup', what: 'some english text' })).toBe('Kurulum gerekiyor');
  });
});

describe('passwordless-seat listener copy', () => {
  it('names the listener state behind a seat failure', () => {
    const s = seatLabel({
      state: 'error',
      reason: 'loopback_listener_unavailable',
      detail: 'step=connecting reason=loopback_listener_unavailable listener=loopback_listener:unavailable:firewall_off'
    });
    expect(s).toContain('Güvenlik Duvarı');
    expect(seatLabel({ state: 'error', reason: 'loopback_listener_unavailable', detail: 'listener=loopback_listener:pending_restart:33389' })).toContain(
      'yeniden başlaması bekleniyor'
    );
    expect(seatLabel({ state: 'error', reason: 'nla_required', detail: '' })).toContain('Gelişmiş');
  });

  it('labels probe notes with a port', () => {
    expect(noteLabel('loopback_listener:ready:33389')).toContain('33389');
    expect(loopbackNoteLabel('loopback_listener_exposed:33389')).toContain('DİKKAT');
    expect(noteLabel('loopback_listener:unavailable:restart_needs_reboot')).toContain('yeniden başlatın');
    expect(loopbackNoteLabel('other')).toBeNull();
  });

  it('labels backend probe notes in Turkish', () => {
    expect(noteLabel('kiosk_shell_seat_aware_apps_only')).toContain('Kiosk');
    expect(noteLabel('two_admin_sessions_only')).toContain('iki yönetici');
    expect(noteLabel('loopback_listener_created')).toContain('oluşturuldu');
  });

  it('says what each session mode really is (spec 35 §6.2)', () => {
    expect(noteLabel('fus_inactive_frozen')).toContain('donar');
    expect(noteLabel('fus_audio_per_user')).toContain('ses aygıtını');
    expect(noteLabel('kiosk_audio_shared')).toContain('1. koltuğun');
    expect(t.backendNames.rds_loopback).toContain('Eşzamanlı tam oturumlar');
    expect(t.backendNames.fast_user_switch).toContain('sırayla');
    expect(t.backendNames.desktops).toContain('Eşzamanlı kiosk koltukları');
    expect(issueText.hide_fsu_with_fast_user_switch).toContain('aynı anda bir koltuk');
  });

  it('validates port and accounts', () => {
    expect(loopbackPortValid(33389)).toBe(true);
    for (const p of [80, 3389, 70000, 1.5]) expect(loopbackPortValid(p)).toBe(false);
    expect(parseAccountList(' ayse\nEV\\mehmet, AYSE\n\n')).toEqual(['ayse', 'EV\\mehmet']);
    expect(accountEntryValid('EV\\mehmet')).toBe(true);
    for (const a of ['', 'x@ev.local', 'a\\b\\c', '\\x']) expect(accountEntryValid(a)).toBe(false);
  });
});
