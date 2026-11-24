// SPDX-License-Identifier: GPL-3.0-only
// Kiosk seats (spec 35 §5.1 U3): config editing and the local checks that mirror seat-core.
import { describe, expect, it } from 'vitest';
import { demoConfig } from './demo.ts';
import { addSeat, isAppUserModelId, kioskAccountOf, kioskAudio, kioskPerSeatAccounts, kioskSeatOneDesktop, localIssues, setKioskPerSeatAccounts, setKioskSeatOneDesktop, setKioskShell } from './model.ts';
import { issueText, noteLabel, t } from './i18n.ts';
import type { DeviceView } from './types.ts';

describe('kiosk shell config', () => {
  it('cleans rows and clamps numbers', () => {
    const c = setKioskShell(demoConfig(), 2, {
      apps: [
        { title: '', command: ' C:\\Windows\\notepad.exe ', autostart: true, monitor: 2.7 },
        { title: 'Boş', command: '  ', autostart: false, monitor: 0 },
        { title: 'Web', command: 'C:\\Web\\web.exe', args: ['--kiosk'], autostart: false, monitor: -3 }
      ],
      idle_reset_minutes: 99999
    });
    const k = c.seats.find((s) => s.number === 2)!.local?.kiosk_shell!;
    expect(k.apps).toEqual([
      { title: 'notepad', command: 'C:\\Windows\\notepad.exe', autostart: true, monitor: 2 },
      { title: 'Web', command: 'C:\\Web\\web.exe', args: ['--kiosk'], autostart: false, monitor: 0 }
    ]);
    expect(k.idle_reset_minutes).toBe(1440);
    const off = setKioskShell(c, 2, null);
    expect(off.seats.find((s) => s.number === 2)!.local?.kiosk_shell).toBeUndefined();
    // Input config is untouched.
    expect(demoConfig().seats.find((s) => s.number === 2)!.local?.kiosk_shell).toBeUndefined();
  });

  it('seat one desktop defaults to on and warns about an ignored seat-one shell', () => {
    let c = demoConfig();
    expect(kioskSeatOneDesktop(c)).toBe(true);
    c = setKioskShell(c, 1, { apps: [], idle_reset_minutes: 0 });
    expect(localIssues(c, null).map((i) => i.code)).toContain('kiosk_shell_ignored_on_seat_one');
    c = setKioskSeatOneDesktop(c, false);
    expect(kioskSeatOneDesktop(c)).toBe(false);
    expect(localIssues(c, null).map((i) => i.code)).not.toContain('kiosk_shell_ignored_on_seat_one');
  });

  it('flags invalid shells like seat-core', () => {
    const c = demoConfig();
    c.seats.find((s) => s.number === 2)!.local = {};
    c.seats.find((s) => s.number === 2)!.local!.kiosk_shell = {
      apps: [{ title: 'x', command: '', autostart: false, monitor: 0 }],
      idle_reset_minutes: 5
    };
    expect(localIssues(c, null).map((i) => i.code)).toContain('invalid_kiosk_shell');
    expect(issueText.invalid_kiosk_shell).toBeTruthy();
    expect(issueText.kiosk_shell_ignored_on_seat_one).toBeTruthy();
    // Service-side check (bug G, 2026-11-24): an app path that is a Store app shortcut.
    expect(issueText.kiosk_app_execution_alias).toContain('AUMID');
  });
});

describe('kiosk parity 2026-11-17', () => {
  it('keeps packaged apps and the full-screen flag', () => {
    const c = setKioskShell(demoConfig(), 2, {
      apps: [
        { title: '', command: '', app_id: ' Microsoft.WindowsCalculator_8wekyb3d8bbwe!App ', autostart: false, monitor: 0 },
        { title: 'Oyun', command: 'C:\\Games\\g.exe', autostart: true, monitor: 0, fullscreen: true }
      ],
      idle_reset_minutes: 0
    });
    const k = c.seats.find((s) => s.number === 2)!.local?.kiosk_shell!;
    expect(k.apps).toEqual([
      { title: 'App', command: '', app_id: 'Microsoft.WindowsCalculator_8wekyb3d8bbwe!App', autostart: false, monitor: 0 },
      { title: 'Oyun', command: 'C:\\Games\\g.exe', autostart: true, monitor: 0, fullscreen: true }
    ]);
    expect(localIssues(c, null).map((i) => i.code)).not.toContain('invalid_kiosk_shell');
    expect(isAppUserModelId('A.B_x!App')).toBe(true);
    for (const bad of ['NoBang', 'a!', '!b', 'a b!c', 'a!b!c']) expect(isAppUserModelId(bad)).toBe(false);
    const broken = setKioskShell(demoConfig(), 2, { apps: [{ title: 'x', command: '', app_id: 'bad', autostart: false, monitor: 0 }], idle_reset_minutes: 0 });
    expect(localIssues(broken, null).map((i) => i.code)).toContain('invalid_kiosk_shell');
  });

  it('per-seat accounts default on and survive the seat-one toggle', () => {
    let c = setKioskShell(demoConfig(), 2, { apps: [{ title: 'a', command: 'C:\\a.exe', autostart: false, monitor: 0 }], idle_reset_minutes: 0 });
    expect(kioskPerSeatAccounts(c)).toBe(true);
    expect(kioskAccountOf(c, 2, false)).toEqual({ kind: 'auto', user: 'PulsarKiosk02' });
    expect(kioskAccountOf(c, 3, false)).toBeNull();
    c.seats.find((s) => s.number === 2)!.account = { kind: 'local', user: 'ayse' };
    expect(kioskAccountOf(c, 2, true)).toEqual({ kind: 'seat', user: 'ayse' });
    expect(kioskAccountOf(c, 2, false)).toEqual({ kind: 'auto', user: 'PulsarKiosk02' });
    c = setKioskPerSeatAccounts(c, false);
    expect(c.settings.kiosk?.accounts).toBe('shared');
    expect(kioskAccountOf(c, 2, true)).toEqual({ kind: 'console' });
    c = setKioskSeatOneDesktop(c, false);
    expect(c.settings.kiosk).toEqual({ seat1_desktop: false, accounts: 'shared' });
    expect(t.kiosk.account.auto('PulsarKiosk02')).toContain('PulsarKiosk02');
    expect(noteLabel('kiosk_account_fallback')).toContain('runs as seat 1’s user');
    expect(noteLabel('kiosk_per_seat_accounts')).toContain('own Windows');
    expect(noteLabel('kiosk_per_seat_accounts')).toContain('not their security');
    expect(noteLabel('kiosk_users_share_desktop')).toContain('share one Windows desktop');
    expect(t.kiosk.account.personalWarning).toContain('personal or domain');
  });
});

describe('kiosk audio', () => {
  const dev = (key: string, flow: 'render' | 'capture', name: string, seat: number, state = 'active', present = true): DeviceView => ({
    device: { key, kind: flow === 'render' ? 'audio_render' : 'audio_capture', name, present, details: { type: 'audio', flow, endpoint_id: `{0.0.${flow === 'render' ? 0 : 1}.00000000}.{${key}}`, form_factor: 'speakers', state } },
    effective: 1 << (seat - 1),
    source: { type: 'explicit' },
    arrived_ms: 0
  });
  const app = (command: string) => ({ title: 'x', command, autostart: false, monitor: 0 });

  it('shows the seat-only endpoints, prefers active ones and flags shared programs', () => {
    let c = setKioskShell(addSeat(demoConfig())!.config, 2, { apps: [app('C:\\Web\\web.exe')], idle_reset_minutes: 0 });
    const devices = [dev('a', 'render', 'Kapalı hoparlör', 2, 'unplugged'), dev('b', 'render', 'Kulaklık', 2), dev('c', 'capture', 'Mikrofon', 3), dev('d', 'render', 'Yok', 2, 'active', false)];
    expect(kioskAudio(c, 2, devices, ['kiosk_audio_per_app'])).toEqual({ state: 'routed', render: 'Kulaklık', capture: null, sharedWith: [] });
    expect(kioskAudio(c, 3, devices, [])).toBeNull();
    // Seat 1 as the normal desktop is never a kiosk seat.
    c = setKioskShell(c, 1, { apps: [app('C:\\Web\\web.exe')], idle_reset_minutes: 0 });
    expect(kioskAudio(c, 1, devices, [])).toBeNull();
    c = setKioskShell(c, 3, { apps: [app(' c:\\web\\WEB.exe ')], idle_reset_minutes: 0 });
    expect(kioskAudio(c, 2, devices, [])?.sharedWith).toEqual([3]);
    expect(kioskAudio(c, 3, devices, [])?.state).toBe('routed');
    expect(kioskAudio(c, 3, [], [])?.state).toBe('no_endpoint');
    expect(kioskAudio(c, 2, devices, ['kiosk_audio_shared'])?.state).toBe('unsupported');
  });

  it('has copy for every state', () => {
    expect(t.kiosk.audio.routed('Kulaklık', null)).toBe('Sound: output Kulaklık');
    expect(t.kiosk.audio.routed('A', 'B')).toContain('microphone B');
    expect(t.kiosk.audio.noEndpoint).toContain('seat 1’s');
    expect(t.kiosk.audio.unsupported).toContain('Windows');
    expect(t.kiosk.audio.sharedApp([3, 4])).toContain('seat 3, 4');
    expect(t.kiosk.limits).toContain('undocumented');
    expect(noteLabel('kiosk_audio_per_app')).toContain('per-app');
  });
});

describe('kiosk copy', () => {
  it('states the hard limits and never offers Ctrl+Alt+Del as a seat key', () => {
    // Ctrl+Alt+Del and the system hotkeys affect every seat (spec 35 §4 U3).
    expect(t.kiosk.limits).toContain('Ctrl+Alt+Del');
    expect(t.kiosk.limits).toContain('Ctrl+Alt+Home');
    expect(t.kiosk.limits).toContain('Windows key');
    // Without the filter the mice leak too; the intro is qualified.
    expect(t.kiosk.seatOneDesktopHint).toContain('kiosk mice');
    expect(t.kiosk.intro).toContain('input filter');
    expect(t.kiosk.backendOnly.length).toBeGreaterThan(0);
  });
});
