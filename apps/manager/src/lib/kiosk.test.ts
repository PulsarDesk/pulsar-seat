// SPDX-License-Identifier: GPL-3.0-only
// Kiosk seats (spec 35 §5.1 U3): config editing and the local checks that mirror seat-core.
import { describe, expect, it } from 'vitest';
import { demoConfig } from './demo.ts';
import { kioskSeatOneDesktop, localIssues, setKioskSeatOneDesktop, setKioskShell } from './model.ts';
import { issueText, t } from './i18n.ts';

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
  });
});

describe('kiosk copy', () => {
  it('states the hard limits and never offers Ctrl+Alt+Del as a seat key', () => {
    // Ctrl+Alt+Del and the system hotkeys affect every seat (spec 35 §4 U3).
    expect(t.kiosk.limits).toContain('Ctrl+Alt+Del');
    expect(t.kiosk.limits).toContain('Ctrl+Alt+Home');
    expect(t.kiosk.limits).toContain('Windows tuşu');
    // Without the filter the mice leak too; the intro is qualified.
    expect(t.kiosk.seatOneDesktopHint).toContain('fareleri');
    expect(t.kiosk.intro).toContain('giriş filtresi');
    expect(t.kiosk.backendOnly.length).toBeGreaterThan(0);
  });
});
