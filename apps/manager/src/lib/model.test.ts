// SPDX-License-Identifier: GPL-3.0-only
import { describe, expect, it } from 'vitest';
import { demoConfig, demoInventory } from './demo.ts';
import * as M from './mask.ts';
import {
  accountError,
  addSeat,
  addedMonitorsToSpare,
  buildCanvas,
  clearDeviceMask,
  coerceMask,
  dropMasksBeyondSeats,
  effectiveMasks,
  emptyHubs,
  localIssues,
  moveDevice,
  nextSeatNumber,
  removeSeat,
  renameSeat,
  sameConfig,
  seatHealth,
  seatNameError,
  seatOneFirstMonitorAdvice,
  setAutostart,
  setDeviceMask,
  setSoftwareCursor,
  setKeyMask,
  portKey,
  portOfLocation,
  containerKey,
  isAbsoluteWindowsPath,
  parseArgs,
  formatArgs,
  setStartupApps,
  setClassDefaultClasses
} from './model.ts';
import { EXPERIMENTAL_WARNING_VERSION, SHARED_MASK, type Config, type Device, type DeviceView } from './types.ts';

const inv = demoInventory();
const dev = (key: string): Device => inv.find((d) => d.key === key)!;
const views = (devices = inv): DeviceView[] =>
  devices.map((device, i) => ({ device, effective: 0, source: { type: 'default' }, arrived_ms: i }));

const KBD_USB = 'kbd;loc=USB(1-2)';
const KBD_HUB = 'kbd;loc=USB(2-1-1)';
const HUB = 'hub;loc=USB(2-1)';
const MON_A = 'mon;loc=PCIROOT(0)#GPU(1);sub=t2';
const MON_LAPTOP = 'mon;loc=PCIROOT(0)#GPU(1);sub=t1';

describe('seats', () => {
  it('adds the lowest free number up to 16 and never removes seat 1', () => {
    let c = demoConfig();
    expect(nextSeatNumber(c)).toBe(3);
    const r = addSeat(c, '  Çocuk  ')!;
    expect(r.seat).toBe(3);
    expect(r.config.seats[2].name).toBe('Çocuk');
    c = r.config;
    for (let i = 0; i < 13; i++) c = addSeat(c)!.config;
    expect(c.seats).toHaveLength(16);
    expect(addSeat(c)).toBeNull();
    expect(removeSeat(c, 1)).toBe(c);
  });

  it('removing a seat sends its devices to the spare pool and keeps shared', () => {
    let c = demoConfig();
    c = setDeviceMask(c, dev('aout;inst=SPK0'), SHARED_MASK);
    const out = removeSeat(c, 2);
    expect(out.seats.map((s) => s.number)).toEqual([1]);
    expect(out.devices.find((d) => d.key === HUB)!.mask).toBe(0);
    expect(out.devices.find((d) => d.key === 'aout;inst=SPK0')!.mask).toBe(SHARED_MASK);
  });

  it('seat 1 always starts; names are validated', () => {
    let c = setAutostart(demoConfig(), 1, false);
    expect(c.seats[0].autostart).toBe(true);
    c = setAutostart(c, 2, false);
    expect(c.seats[1].autostart).toBe(false);
    expect(renameSeat(c, 2, '  Oyun  ').seats[1].name).toBe('Oyun');
    expect(seatNameError(' ')).toBe('empty');
    expect(seatNameError('x'.repeat(65))).toBe('too_long');
    expect(seatNameError('a\u0007b')).toBe('control');
    expect(seatNameError('Mutfak 2')).toBeNull();
  });

  it('checks accounts like the service', () => {
    expect(accountError({ kind: 'logon_screen' })).toBeNull();
    expect(accountError({ kind: 'local', user: 'ali' })).toBeNull();
    expect(accountError({ kind: 'local', user: 'ev\\ali' })).toBe('user');
    expect(accountError({ kind: 'domain', domain: '', user: 'ali' })).toBe('domain');
  });
});

describe('device assignment', () => {
  it('exclusive kinds keep one seat; network is always shared', () => {
    expect(coerceMask('keyboard', 0b110)).toBe(0b10);
    expect(coerceMask('mouse', SHARED_MASK)).toBe(1);
    expect(coerceMask('audio_render', 0b110)).toBe(0b110);
    expect(coerceMask('network', 0)).toBe(SHARED_MASK);
  });

  it('creates and clears explicit records with the device identity', () => {
    const c = setDeviceMask(demoConfig(), dev(KBD_USB), M.bit(2));
    const a = c.devices.find((d) => d.key === KBD_USB)!;
    expect(a.mask).toBe(2);
    expect(a.kind).toBe('keyboard');
    expect(a.hardware_id).toContain('HID');
    expect(clearDeviceMask(c, KBD_USB).devices.some((d) => d.key === KBD_USB)).toBe(false);
  });

  it('drag and drop moves devices and drops to spare', () => {
    let c = demoConfig();
    c = moveDevice(c, dev(KBD_USB), 1, 1, 2);
    expect(c.devices.find((d) => d.key === KBD_USB)!.mask).toBe(2);
    c = moveDevice(c, dev(KBD_USB), 2, 2, 'system');
    expect(c.devices.find((d) => d.key === KBD_USB)!.mask).toBe(0);
    // Shareable: keeps the other seats.
    const spk = dev('aout;inst=SPK0');
    c = setDeviceMask(c, spk, 0b011);
    c = moveDevice(c, spk, 0b011, 1, 2);
    expect(c.devices.find((d) => d.key === spk.key)!.mask).toBe(0b010);
    // A shared device dropped on a seat becomes that seat's own.
    c = moveDevice(c, spk, SHARED_MASK, 'system', 1);
    expect(c.devices.find((d) => d.key === spk.key)!.mask).toBe(1);
  });

  it('effective masks follow hubs like seat-core', () => {
    const c = demoConfig();
    const eff = effectiveMasks(c, inv);
    // Keyboard on the seat-2 hub inherits seat 2.
    expect(eff.get(KBD_HUB)).toEqual({ mask: 2, source: { type: 'inherited', hub: HUB } });
    // Headset on the same hub (shareable) inherits too.
    expect(eff.get('aout;loc=USB(2-1-3)')!.mask).toBe(2);
    // Unassigned USB keyboard: default seat 1.
    expect(eff.get(KBD_USB)).toEqual({ mask: 1, source: { type: 'default' } });
    // Network: always shared.
    expect(eff.get('net;inst=WLAN0')!.mask).toBe(SHARED_MASK);
    // A shared hub does not pull exclusive children off their default.
    const shared = setDeviceMask(c, dev(HUB), SHARED_MASK);
    expect(effectiveMasks(shared, inv).get(KBD_HUB)!.source).toEqual({ type: 'default' });
  });

  it('software cursor option is stored on the monitor record', () => {
    const c = setSoftwareCursor(demoConfig(), dev(MON_A), true, 1);
    expect(c.devices.find((d) => d.key === MON_A)!.options).toEqual({ software_cursor: true });
    const back = setSoftwareCursor(c, dev(MON_A), false, 1);
    expect(back.devices.find((d) => d.key === MON_A)!.options).toBeUndefined();
  });
});

describe('canvas', () => {
  it('puts spare and shared devices in the system container', () => {
    const canvas = buildCanvas(demoConfig(), views(), { sharedInSeats: false, showAllHubs: true, visibleSeats: M.ALL_SEATS_BITS });
    const sys = canvas.system.map((i) => i.view.device.key);
    expect(sys).toContain('aout;inst=SPK0');
    expect(sys).toContain('net;inst=WLAN0');
    const two = canvas.seats.get(2)!.map((i) => i.view.device.key);
    expect(two).toContain(KBD_HUB);
    expect(two).toContain('mon;loc=PCIROOT(0)#GPU(2);sub=t1');
    // Category order: monitors first.
    expect(canvas.seats.get(1)![0].view.device.kind).toBe('monitor');
  });

  it('can show shared devices inside seats and hide empty hubs', () => {
    const canvas = buildCanvas(demoConfig(), views(), { sharedInSeats: true, showAllHubs: false, visibleSeats: M.ALL_SEATS_BITS });
    expect(canvas.seats.get(2)!.some((i) => i.shared && i.view.device.key === 'aout;inst=SPK0')).toBe(true);
    // USB(2) holds only a hub that has devices: not empty. Nothing hidden in the demo.
    expect(emptyHubs(inv).size).toBe(0);
    const lonely: Device = { key: 'hub;x', kind: 'usb_hub', name: 'Boş göbek', present: true, details: { type: 'none' } };
    expect(emptyHubs([...inv, lonely]).has('hub;x')).toBe(true);
  });

  it('seat colours: grey, red, yellow, green', () => {
    let c = demoConfig();
    expect(seatHealth(c, 2, views()).colour).toBe('green');
    expect(seatHealth(c, 1, views()).colour).toBe('green');
    c = setAutostart(c, 2, false);
    expect(seatHealth(c, 2, views())).toEqual({ colour: 'grey', readiness: 'not_starting' });
    c = setAutostart(c, 2, true);
    c = setDeviceMask(c, dev('mon;loc=PCIROOT(0)#GPU(2);sub=t1'), 0);
    expect(seatHealth(c, 2, views()).readiness).toBe('no_display');
    c = setDeviceMask(c, dev(MON_A), 2);
    c = setDeviceMask(c, dev(KBD_HUB), 0);
    expect(seatHealth(c, 2, views())).toEqual({ colour: 'yellow', readiness: 'no_keyboard' });
  });
});

describe('validation mirror', () => {
  it('flags seat 1 without display and exclusive devices on several seats', () => {
    // 1.2 is unassigned (default seat 1), so move it away too.
    let c = setDeviceMask(setDeviceMask(demoConfig(), dev(MON_LAPTOP), 0), dev(MON_A), 2);
    c = { ...c, devices: [...c.devices, { key: KBD_USB, kind: 'keyboard', mask: 3, name: 'x' }] };
    const codes = localIssues(c, inv).map((i) => i.code);
    expect(codes).toContain('seat_one_no_display');
    expect(codes).toContain('exclusive_on_several_seats');
  });

  it('flags the experimental backend without acknowledgement and bad timings', () => {
    const c = demoConfig();
    c.settings.backends.session = 'termsrv_wrapper';
    c.settings.timings.seat_start_timeout_s = 10;
    c.settings.input.switch_hotkey_scan = 0x1d;
    const codes = localIssues(c, null).map((i) => i.code);
    expect(codes).toEqual(expect.arrayContaining(['experimental_not_acknowledged', 'timeout_out_of_range', 'invalid_hotkey']));
    c.settings.backends.experimental_ack = EXPERIMENTAL_WARNING_VERSION;
    expect(localIssues(c, null).map((i) => i.code)).not.toContain('experimental_not_acknowledged');
  });

  it('hiding fast user switching conflicts with the fast_user_switch backend', () => {
    const c = demoConfig();
    c.settings.tweaks.hide_fast_user_switching = true;
    c.settings.backends.session = 'rds_loopback';
    expect(localIssues(c, null).map((i) => i.code)).not.toContain('hide_fsu_with_fast_user_switch');
    c.settings.backends.session = 'fast_user_switch';
    const i = localIssues(c, null).find((x) => x.code === 'hide_fsu_with_fast_user_switch');
    expect(i?.severity).toBe('error');
  });

  it('same account on two seats only matters under deny', () => {
    const c = demoConfig();
    c.seats[0].account = { kind: 'local', user: 'Ali' };
    c.seats[1].account = { kind: 'local', user: 'ali' };
    expect(localIssues(c, null).map((i) => i.code)).not.toContain('same_account_on_seats');
    c.settings.same_account = 'deny';
    expect(localIssues(c, null).map((i) => i.code)).toContain('same_account_on_seats');
  });

  it('dirty check ignores ordering; masks above the seats go spare', () => {
    const a = demoConfig();
    const b = { ...a, devices: [...a.devices].reverse() };
    expect(sameConfig(a, b)).toBe(true);
    const c = { ...a, devices: [...a.devices, { key: 'kbd;z', kind: 'keyboard' as const, mask: 0b1000, name: 'z' }] };
    const r = dropMasksBeyondSeats(c);
    expect(r.moved).toEqual(['kbd;z']);
    expect(r.config.devices.find((d) => d.key === 'kbd;z')!.mask).toBe(0);
  });

  it('advises seat 1 to use monitor 1.1 and sends new monitors to spare', () => {
    expect(seatOneFirstMonitorAdvice(demoConfig(), views())).toBeNull();
    const c = setDeviceMask(setDeviceMask(demoConfig(), dev(MON_LAPTOP), 2), dev(MON_A), 1);
    expect(seatOneFirstMonitorAdvice(c, views())).toBe(MON_LAPTOP);
    const spare = addedMonitorsToSpare(demoConfig(), [MON_A, KBD_USB], views());
    expect(spare.devices.find((d) => d.key === MON_A)!.mask).toBe(0);
    expect(spare.devices.some((d) => d.key === KBD_USB)).toBe(false);
  });
});

describe('port and container assignments (spec 70 §5.1, mirror of seat-core assign.rs)', () => {
  const hubLoc = 'PCIROOT(0)#PCI(1400)#USBROOT(0)#USB(2)';
  const base = (key: string, kind: Device['kind'], hub?: string): Device =>
    ({ ...inv.find((d) => d.kind === kind)!, key, kind, parent_hub: hub, container_id: undefined }) as Device;

  it('nearest port wins, a container beats the ports, a shared container keeps the default', () => {
    const hub = `hub;loc=${hubLoc}`;
    const kbd = { ...base(`kbd;loc=${hubLoc}#USB(4)#USBMI(0)`, 'keyboard', hub), container_id: '{ABCDEF01-0000-0000-0000-000000000001}' };
    const stor = base(`stor;loc=${hubLoc}#USB(1);sub=lun0`, 'storage', hub);
    const h = base(hub, 'usb_hub');
    const devices = [h, kbd, stor];
    let c: Config = { ...demoConfig(), devices: [] };
    c = setKeyMask(c, portKey(`${hubLoc}#USB(4)`), 'usb_hub', 2, 'p4');
    let e = effectiveMasks(c, devices);
    expect(e.get(kbd.key)).toEqual({ mask: 2, source: { type: 'inherited', hub: `port;loc=${hubLoc}#USB(4)` } });
    expect(e.get(stor.key)!.source).toEqual({ type: 'default' });
    c = setKeyMask(c, portKey(hubLoc), 'usb_hub', 4, 'root');
    e = effectiveMasks(c, devices);
    expect(e.get(kbd.key)!.mask).toBe(2);
    expect(e.get(stor.key)!.mask).toBe(4);
    expect(e.get(hub)!.mask).toBe(4);
    const ck = containerKey('{ABCDEF01-0000-0000-0000-000000000001}')!;
    expect(ck).toBe('cont;id=abcdef01-0000-0000-0000-000000000001');
    c = setKeyMask(c, ck, 'other', 4, 'kbd');
    expect(effectiveMasks(c, devices).get(kbd.key)!.mask).toBe(4);
    c = setKeyMask(c, ck, 'other', SHARED_MASK, 'kbd');
    expect(effectiveMasks(c, devices).get(kbd.key)).toEqual({ mask: 1, source: { type: 'default' } });
    expect(containerKey('{00000000-0000-0000-FFFF-FFFFFFFFFFFF}')).toBeNull();
    // Spare removes the assignment again.
    expect(setKeyMask(c, ck, 'other', 0, 'kbd').devices.some((d) => d.key === ck)).toBe(false);
  });

  it('port paths end at the last USB(n) segment', () => {
    expect(portOfLocation('A#USB(2)#USB(4)#USBMI(0)')).toBe('A#USB(2)#USB(4)');
    expect(portOfLocation('PCIROOT(0)#PCI(1400)')).toBeNull();
    expect(portKey('a;b')).toBe('port;loc=a_b');
  });
});

describe('startup apps and class defaults', () => {
  it('stores absolute startup programs and parses argument lines', () => {
    expect(isAbsoluteWindowsPath('C:\\Oyun\\a.exe')).toBe(true);
    expect(isAbsoluteWindowsPath('\\\\srv\\pay\\a.exe')).toBe(true);
    expect(isAbsoluteWindowsPath('notepad.exe')).toBe(false);
    expect(parseArgs('-lan "C:\\Kayıt Yeri" x')).toEqual(['-lan', 'C:\\Kayıt Yeri', 'x']);
    expect(parseArgs('a ""')).toEqual(['a', '']);
    expect(formatArgs(['-lan', 'a b', ''])).toBe('-lan "a b" ""');
    let c = setStartupApps(demoConfig(), 2, [{ command: ' C:\\a.exe ', args: ['-x'] }, { command: '  ', args: [] }]);
    expect(c.seats.find((s) => s.number === 2)!.startup_apps).toEqual([{ command: 'C:\\a.exe', args: ['-x'] }]);
    c = setStartupApps(c, 2, []);
    expect(c.seats.find((s) => s.number === 2)!.startup_apps).toBeUndefined();
  });

  it('keeps the class choice and drops it when empty', () => {
    let c = setClassDefaultClasses(demoConfig(), ['cdrom', 'volume', 'cdrom']);
    expect(c.settings.devices.class_default_classes).toEqual(['cdrom', 'volume']);
    c = setClassDefaultClasses(c, []);
    expect(c.settings.devices.class_default_classes).toBeUndefined();
  });
});
