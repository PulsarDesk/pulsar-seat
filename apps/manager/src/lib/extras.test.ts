// SPDX-License-Identifier: GPL-3.0-only
// Pictures, per-seat addresses, outputs, log levels and the column-major grid.
import { describe, expect, it } from 'vitest';
import { demoConfig, demoInventory } from './demo.ts';
import {
  clearAllPictures,
  gridRows,
  isUsableIpv4,
  localIssues,
  normalize,
  setIpBinding,
  setIpPrograms,
  setLogLevels,
  setMonitorOutputs,
  setOutputSettings,
  setPicture
} from './model.ts';
import { categoryOf, offName, parsePicture, pictureValue, selectableUserPictures, userPictureUrl } from './pictures.ts';
import { MAX_IP_PROGRAMS } from './types.ts';

const inv = demoInventory();
const mon = inv.find((d) => d.kind === 'monitor')!;
const kbd = inv.find((d) => d.kind === 'keyboard')!;

describe('pictures', () => {
  it('parses and writes picture references', () => {
    expect(parsePicture(undefined)).toEqual({ type: 'default' });
    expect(parsePicture('builtin:tv')).toEqual({ type: 'builtin', id: 'tv' });
    expect(parsePicture('builtin:unknown')).toEqual({ type: 'default' });
    expect(parsePicture('user:a.png')).toEqual({ type: 'user', name: 'a.png' });
    expect(pictureValue({ type: 'builtin', id: 'pen' })).toBe('builtin:pen');
    expect(pictureValue({ type: 'default' })).toBeUndefined();
    expect(categoryOf('webcam')).toBe('video_capture');
    expect(categoryOf('usb_hub')).toBeNull();
  });

  it('pairs "not in use" files and falls back to the default', () => {
    const list = [
      { name: 'k.png', data_url: 'data:on' },
      { name: 'k_off.png', data_url: 'data:off' },
      { name: 'x.png', data_url: 'data:x' }
    ];
    expect(offName('k.png')).toBe('k_off.png');
    expect(selectableUserPictures(list).map((p) => p.name)).toEqual(['k.png', 'x.png']);
    expect(userPictureUrl(list, 'k.png', true)).toBe('data:on');
    expect(userPictureUrl(list, 'k.png', false)).toBe('data:off');
    expect(userPictureUrl(list, 'x.png', false)).toBeNull();
    expect(userPictureUrl(undefined, 'x.png', true)).toBeNull();
  });

  it('stores and clears custom pictures', () => {
    let c = setPicture(demoConfig(), kbd, 'builtin:keyboard-compact', 2);
    expect(c.devices.find((a) => a.key === kbd.key)?.options?.picture).toBe('builtin:keyboard-compact');
    c = clearAllPictures(c);
    expect(c.devices.find((a) => a.key === kbd.key)?.options).toBeUndefined();
  });
});

describe('per-seat addresses', () => {
  it('validates IPv4 like seat-core', () => {
    expect(isUsableIpv4('192.168.1.20')).toBe(true);
    expect(isUsableIpv4('10.0.0.1')).toBe(true);
    expect(isUsableIpv4('0.0.0.0')).toBe(false);
    expect(isUsableIpv4('255.255.255.255')).toBe(false);
    expect(isUsableIpv4('224.0.0.1')).toBe(false);
    expect(isUsableIpv4('300.1.1.1')).toBe(false);
    expect(isUsableIpv4('01.2.3.4')).toBe(false);
    expect(isUsableIpv4('1.2.3')).toBe(false);
  });

  it('stores the binding and the shared list, with the same issues as the service', () => {
    let c = normalize(demoConfig());
    c = setIpBinding(c, 1, { address: '192.168.1.20', rule: 'all_programs' });
    c = setIpBinding(c, 2, { address: '192.168.1.20', rule: 'only_listed' });
    expect(c.seats[1].ip_binding).toEqual({ address: '192.168.1.20', rule: 'only_listed' });
    expect(localIssues(c, null).map((i) => i.code)).toEqual(['duplicate_ip_address']);
    c = setIpBinding(c, 2, null);
    expect(c.seats[1].ip_binding).toBeUndefined();
    c = setIpPrograms(c, [{ title: ' Oyun ', path: ' C:\\Oyun\\oyun.exe ', enabled: true }]);
    expect(c.settings.ip_programs).toEqual([{ title: 'Oyun', path: 'C:\\Oyun\\oyun.exe', enabled: true }]);
    c = setIpPrograms(c, [{ title: 'x', path: ' ', enabled: true }]);
    expect(localIssues(c, null).map((i) => i.code)).toContain('invalid_ip_program');
    const long = Array.from({ length: MAX_IP_PROGRAMS + 1 }, (_, i) => ({ title: `${i}`, path: `C:\\p${i}.exe`, enabled: true }));
    expect(localIssues(setIpPrograms(c, long), null).map((i) => i.code)).toContain('too_many_ip_programs');
    expect(localIssues(setIpBinding(c, 1, { address: 'x', rule: 'all_programs' }), null).map((i) => i.code)).toContain('invalid_ip_address');
  });
});

describe('outputs and log levels', () => {
  it('needs the clone switch for several outputs', () => {
    let c = setMonitorOutputs(normalize(demoConfig()), mon, [3, 1, 3], 1);
    expect(c.devices.find((a) => a.key === mon.key)?.options?.outputs).toEqual([1, 3]);
    expect(localIssues(c, null).map((i) => i.code)).toContain('clone_not_allowed');
    c = setOutputSettings(c, { allow_clone: true, force_assign_at_start: true });
    expect(localIssues(c, null).map((i) => i.code)).not.toContain('clone_not_allowed');
    c = setMonitorOutputs(c, mon, [], 1);
    expect(c.devices.find((a) => a.key === mon.key)?.options?.outputs).toBeUndefined();
  });

  it('fills defaults for older services and stores log levels', () => {
    const c = normalize(demoConfig());
    expect(c.settings.log).toEqual({ info: true, debug: false, trace: false });
    expect(c.settings.outputs).toEqual({ force_assign_at_start: false, allow_clone: false });
    expect(setLogLevels(c, { info: true, debug: true, trace: false }).settings.log?.debug).toBe(true);
  });

  it('stores the mode-follow choice', () => {
    const c = setOutputSettings(normalize(demoConfig()), { allow_clone: false, force_assign_at_start: false, follow_session_mode: true });
    expect(c.settings.outputs?.follow_session_mode).toBe(true);
    expect(JSON.parse(JSON.stringify(setOutputSettings(c, { allow_clone: false, force_assign_at_start: false }))).settings.outputs.follow_session_mode).toBeUndefined();
  });
});

describe('column-major grid', () => {
  it('computes rows from the width', () => {
    expect(gridRows(0, 500, 100)).toBe(1);
    expect(gridRows(7, 300, 100)).toBe(3);
    expect(gridRows(7, 0, 100)).toBe(7);
    expect(gridRows(2, 1000, 100)).toBe(1);
  });
});

describe('fast user switching accounts', () => {
  it('needs one account per seat under fast_user_switch', () => {
    const c = normalize(demoConfig());
    c.settings.same_account = 'warn';
    c.seats = c.seats.slice(0, 2).map((s) => ({ ...s, account: { kind: 'local', user: 'Ali' } as typeof s.account }));
    if (c.seats.length < 2) return;
    expect(localIssues(c, null).map((i) => i.code)).not.toContain('same_account_fast_user_switch');
    c.settings.backends.session = 'fast_user_switch';
    expect(localIssues(c, null).map((i) => i.code)).toContain('same_account_fast_user_switch');
  });
});
