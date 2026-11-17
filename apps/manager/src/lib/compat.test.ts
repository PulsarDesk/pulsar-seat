// SPDX-License-Identifier: GPL-3.0-only
import { fireEvent, render, screen } from '@testing-library/svelte';
import userEvent from '@testing-library/user-event';
import { describe, expect, it, vi } from 'vitest';
import {
  compatIssues,
  dockCountStable,
  denyLogonAllowed,
  envListOk,
  envNameOk,
  objectRuleValid,
  proposeDockSeats,
  unusedInstallClasses,
  usbDocks,
  windowsUpdateValid
} from './compat.ts';
import { assignDocks } from './docks.ts';
import { demoConfig } from './demo.ts';
import { t } from './i18n.ts';
import { localIssues, newSeat, setSeatCompat } from './model.ts';
import type { Config, Device, DeviceKind, DeviceView, Issue } from './types.ts';
import SeatCompatDialog from './components/SeatCompatDialog.svelte';

const issue = (severity: Issue['severity'], code: string, extra: Partial<Issue> = {}): Issue => ({ severity, code, ...extra });

function dev(key: string, kind: DeviceKind, extra: Partial<Device> = {}): Device {
  return { key, kind, name: key, present: true, details: { type: 'none' }, ...extra };
}

function view(d: Device, effective = 0, container_key?: string): DeviceView {
  return { device: d, effective, source: { type: 'default' }, arrived_ms: 0, container_key };
}

function twoSeats(): Config {
  const c = demoConfig();
  if (!c.seats.some((s) => s.number === 2)) c.seats.push(newSeat(2));
  c.seats.find((s) => s.number === 2)!.account = { kind: 'local', user: 'ali' };
  return c;
}

describe('compat validation mirror', () => {
  it('environment names and values follow seat-core', () => {
    expect(envNameOk('GAME_PROFILE')).toBe(true);
    for (const bad of ['', 'Path', 'temp', 'PULSAR_SEAT_X', '1A', 'A B', 'A=B']) expect(envNameOk(bad), bad).toBe(false);
    expect(envListOk([{ name: 'A', value: '1' }, { name: 'a', value: '2' }])).toBe(false);
    expect(envListOk([{ name: 'A', value: 'x\ny' }])).toBe(false);
    expect(envListOk(Array.from({ length: 33 }, (_, i) => ({ name: `V${i}`, value: '' })))).toBe(false);
  });

  it('windows update ranges and active hours', () => {
    const w = { enabled: true, feature_defer_days: 365, quality_defer_days: 30, active_hours_start: 20, active_hours_end: 8, no_auto_restart_with_users: true };
    expect(windowsUpdateValid(w)).toBe(true);
    expect(windowsUpdateValid({ ...w, active_hours_end: 16 })).toBe(false);
    expect(windowsUpdateValid({ ...w, active_hours_end: null })).toBe(false);
    expect(windowsUpdateValid({ ...w, quality_defer_days: 31 })).toBe(false);
    expect(windowsUpdateValid(undefined)).toBe(true);
  });

  it('object rules', () => {
    expect(objectRuleValid({ exe: 'Launcher.exe', names: ['One*'] })).toBe(true);
    for (const [exe, name] of [['C:\\x\\a.exe', 'n'], ['a.dll', 'n'], ['a.exe', '*'], ['a.exe', 'Global\\n']]) {
      expect(objectRuleValid({ exe, names: [name] }), `${exe} ${name}`).toBe(false);
    }
  });

  it('deny log on locally only for RDP-signed-in seats with an account', () => {
    const c = twoSeats();
    c.settings.backends.session = 'rds_loopback';
    expect(denyLogonAllowed(c, 2)).toBe(true);
    expect(denyLogonAllowed(c, 1)).toBe(false);
    c.settings.backends.session = 'fast_user_switch';
    expect(denyLogonAllowed(c, 2)).toBe(false);
    c.settings.backends.session = 'rds_loopback';
    const d = setSeatCompat(c, 2, [{ name: 'Path', value: 'x' }], true);
    const codes = compatIssues(d, issue).map((i) => i.code);
    expect(codes).toContain('invalid_seat_environment');
    expect(codes).not.toContain('deny_local_logon_unsupported');
    expect(localIssues(d, null).map((i) => i.code)).toContain('invalid_seat_environment');
    d.settings.backends.session = 'desktops';
    expect(compatIssues(d, issue).map((i) => i.code)).toContain('deny_local_logon_unsupported');
  });

  it('setSeatCompat removes empty values', () => {
    const c = setSeatCompat(setSeatCompat(twoSeats(), 2, [{ name: 'A', value: '1' }], true), 2, [], false);
    const s = c.seats.find((x) => x.number === 2)!;
    expect(s.environment).toBeUndefined();
    expect(s.deny_local_logon).toBeUndefined();
  });
});

describe('device-install deny suggestion', () => {
  it('suggests classes no seat uses', () => {
    const c = twoSeats();
    c.devices = [];
    const views = [view(dev('prn;1', 'printer'), 2), view(dev('cam;1', 'webcam'), 0)];
    const unused = unusedInstallClasses(c, views);
    expect(unused).not.toContain('printer');
    expect(unused).toContain('camera');
    expect(unused).toContain('wpd');
  });
});

describe('USB dock wizard', () => {
  function dock(n: number, mon: number): DeviceView[] {
    const hub = dev(`hub;${n}`, 'usb_hub', { container_id: `c${n}`, serial: `SN${n}` });
    return [
      view(hub, 0, `cont;id=c${n}`),
      view(dev(`mon;${n}`, 'monitor', { container_id: `c${n}` }), mon),
      view(dev(`kbd;${n}`, 'keyboard', { parent_hub: `hub;${n}` }), 0),
      view(dev(`spk;${n}`, 'audio_render', { container_id: `c${n}` }), 0)
    ];
  }

  it('finds docks, folds nested hubs and proposes seats', () => {
    const inner = dev('hub;9', 'usb_hub', { parent_hub: 'hub;1' });
    const views = [...dock(1, 0), ...dock(2, 0b100), view(inner), view(dev('mon;x', 'monitor'), 1)];
    const docks = usbDocks(views);
    expect(docks.map((d) => d.label)).toEqual(['SN1', 'SN2']);
    expect(docks[0].monitors.length).toBe(1);
    expect(docks[0].inputs.length + docks[0].audio.length).toBe(2);
    expect(docks[1].seat).toBe(3);
    expect(proposeDockSeats(docks, [3], [1, 2, 3])).toEqual([2, 3]);
    expect(proposeDockSeats(docks, [2, 3], [1, 2, 3])).toEqual([null, 3]);
    expect(dockCountStable([1, 2])).toBe(false);
    expect(dockCountStable([1, 2, 2])).toBe(true);
  });

  it('assigns every device of a dock to the chosen or a new seat', () => {
    const views = [...dock(1, 0), ...dock(2, 0)];
    const docks = usbDocks(views);
    const c = twoSeats();
    const before = c.seats.length;
    const r = assignDocks(c, docks, [{ kind: 'seat', seat: 2 }, { kind: 'new' }], ['', 'Dock B']);
    expect(r.assigned).toBe(2);
    expect(r.config.seats.length).toBe(before + 1);
    const newSeatNo = r.config.seats[r.config.seats.length - 1].number;
    expect(r.config.devices.find((d) => d.key === 'cont;id=c1')?.mask).toBe(0b10);
    expect(r.config.devices.find((d) => d.key === 'mon;1')?.mask).toBe(0b10);
    expect(r.config.devices.find((d) => d.key === 'mon;2')?.mask).toBe(1 << (newSeatNo - 1));
    expect(r.config.seats.find((s) => s.number === newSeatNo)?.name).toBe('Dock B');
    const skip = assignDocks(c, docks, [{ kind: 'skip' }, { kind: 'skip' }]);
    expect(skip.assigned).toBe(0);
  });
});

describe('SeatCompatDialog', () => {
  it('saves variables and the deny option', async () => {
    const onSave = vi.fn();
    render(SeatCompatDialog, { seat: 2, environment: [], denyLocalLogon: false, denyAllowed: true, onSave, onCancel: vi.fn() });
    const user = userEvent.setup();
    await user.click(screen.getByRole('button', { name: t.seatCompat.envAdd }));
    await fireEvent.input(screen.getByTestId('env-name'), { target: { value: 'GAME_PROFILE' } });
    await fireEvent.input(screen.getByTestId('env-value'), { target: { value: 'seat2' } });
    await user.click(screen.getByTestId('deny-local-logon'));
    await user.click(screen.getByRole('button', { name: t.save }));
    expect(onSave).toHaveBeenCalledWith([{ name: 'GAME_PROFILE', value: 'seat2' }], true);
  });

  it('refuses a reserved name and explains when deny is not possible', async () => {
    render(SeatCompatDialog, { seat: 2, environment: [{ name: 'Path', value: 'x' }], denyLocalLogon: false, denyAllowed: false, onSave: vi.fn(), onCancel: vi.fn() });
    expect(screen.getByText(t.seatCompat.envInvalid)).toBeInTheDocument();
    expect(screen.getByRole('button', { name: t.save })).toBeDisabled();
    expect(screen.getByText(t.seatCompat.denyUnavailable)).toBeInTheDocument();
    expect(screen.getByTestId('deny-local-logon')).toBeDisabled();
  });
});
