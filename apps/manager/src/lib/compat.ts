// SPDX-License-Identifier: GPL-3.0-only
// Compatibility options (compat-tweaks 2026-11-17): validation mirror of seat-core's
// `compat_checks`, defaults, the device-install deny suggestion ("classes no seat uses")
// and the multi-seat USB dock detection used by the USB dock wizard.

import type {
  Config,
  ConsoleGuard,
  Device,
  DeviceInstallClass,
  DeviceInstallDeny,
  DeviceKey,
  DeviceKind,
  DeviceView,
  EnvVar,
  Issue,
  ObjectRule,
  SeatNumber,
  WindowsUpdatePolicy
} from './types.ts';

export const MAX_SEAT_ENV = 32;
export const MAX_SEAT_ENV_VALUE = 2048;
export const MAX_OBJECT_RULES = 32;
export const MAX_OBJECT_NAMES = 16;
export const MAX_FEATURE_DEFER_DAYS = 365;
export const MAX_QUALITY_DEFER_DAYS = 30;
export const MAX_ACTIVE_HOURS = 18;

/** Same list as `seat_core::config::RESERVED_ENV_NAMES` (upper case). */
export const RESERVED_ENV_NAMES = [
  'PATH',
  'PATHEXT',
  'SYSTEMROOT',
  'WINDIR',
  'COMSPEC',
  'TEMP',
  'TMP',
  'USERPROFILE',
  'USERNAME',
  'USERDOMAIN',
  'APPDATA',
  'LOCALAPPDATA',
  'PROGRAMDATA',
  'PROGRAMFILES',
  'HOMEDRIVE',
  'HOMEPATH',
  '__COMPAT_LAYER',
  'SDL_GAMECONTROLLER_IGNORE_DEVICES',
  'SDL_JOYSTICK_IGNORE_DEVICES',
  'STEAM_MASTER_IPC_NAME_OVERRIDE'
];

export const INSTALL_CLASSES: DeviceInstallClass[] = ['printer', 'image', 'camera', 'wpd', 'modem', 'cdrom', 'ports', 'smart_card_reader'];

export function defaultWindowsUpdate(): WindowsUpdatePolicy {
  return { enabled: false, feature_defer_days: 0, quality_defer_days: 0, active_hours_start: null, active_hours_end: null, no_auto_restart_with_users: false };
}

export function defaultInstallDeny(): DeviceInstallDeny {
  return { classes: [], admins_override: true };
}

/** `settings.console_guard` with the service defaults filled in (`seat_core::config::ConsoleGuard`:
 *  host_lock_off on, seat_one_lock_off off; OSMS parity G4, 2026-11-18). */
export function consoleGuard(g: ConsoleGuard | undefined): Required<ConsoleGuard> {
  return { host_lock_off: g?.host_lock_off ?? true, seat_one_lock_off: g?.seat_one_lock_off ?? false };
}

/** `seat_core::config::env_name_ok`. */
export function envNameOk(name: string): boolean {
  const up = name.toUpperCase();
  return (
    name.length > 0 &&
    name.length <= 255 &&
    !/^[0-9]/.test(name) &&
    /^[A-Za-z0-9_\-.()]+$/.test(name) &&
    !RESERVED_ENV_NAMES.includes(up) &&
    !up.startsWith('PULSAR_SEAT_')
  );
}

/** `seat_core::config::env_value_ok`. */
export function envValueOk(value: string): boolean {
  // eslint-disable-next-line no-control-regex
  return [...value].length <= MAX_SEAT_ENV_VALUE && !/[\u0000-\u001f\u007f-\u009f]/.test(value);
}

export function envListOk(vars: EnvVar[]): boolean {
  const names = vars.map((v) => v.name.toUpperCase());
  return vars.length <= MAX_SEAT_ENV && new Set(names).size === names.length && vars.every((v) => envNameOk(v.name) && envValueOk(v.value));
}

/** Active-hours window length (wraps over midnight). */
export function activeHoursLen(start: number, end: number): number {
  return (((end - start) % 24) + 24) % 24;
}

/** `WindowsUpdatePolicy::is_valid`. */
export function windowsUpdateValid(w: WindowsUpdatePolicy | undefined): boolean {
  if (!w) return true;
  const s = w.active_hours_start ?? null;
  const e = w.active_hours_end ?? null;
  const hours =
    (s === null && e === null) ||
    (s !== null && e !== null && Number.isInteger(s) && Number.isInteger(e) && s >= 0 && s < 24 && e >= 0 && e < 24 && s !== e && activeHoursLen(s, e) <= MAX_ACTIVE_HOURS);
  const days = (d: number, max: number) => Number.isInteger(d) && d >= 0 && d <= max;
  return hours && days(w.feature_defer_days, MAX_FEATURE_DEFER_DAYS) && days(w.quality_defer_days, MAX_QUALITY_DEFER_DAYS);
}

/** `ObjectRule::is_valid`. */
export function objectRuleValid(r: ObjectRule): boolean {
  const exe = r.exe.trim();
  return (
    exe.length > 0 &&
    exe.length <= 260 &&
    exe.toLowerCase().endsWith('.exe') &&
    !/[\\/\t:|]/.test(exe) &&
    r.names.length > 0 &&
    r.names.length <= MAX_OBJECT_NAMES &&
    r.names.every((n) => {
      const t = n.trim();
      // eslint-disable-next-line no-control-regex
      return t.length > 0 && t !== '*' && t.length <= 200 && !/[\\\t|\u0000-\u001f]/.test(t);
    })
  );
}

/** Seats that sign in through Remote Desktop (where "Deny log on locally" is allowed). */
export function denyLogonAllowed(c: Config, seat: SeatNumber): boolean {
  const s = c.seats.find((x) => x.number === seat);
  if (!s || s.account.kind === 'logon_screen') return false;
  if (s.kind === 'remote') return true;
  const b = c.settings.backends.session;
  return seat !== 1 && (b === 'rds_loopback' || b === 'termsrv_wrapper');
}

/** Mirror of seat-core `compat_checks`. */
export function compatIssues(c: Config, issue: (sev: Issue['severity'], code: string, extra?: Partial<Issue>) => Issue): Issue[] {
  const out: Issue[] = [];
  for (const s of c.seats) {
    if (!envListOk(s.environment ?? [])) out.push(issue('error', 'invalid_seat_environment', { seat: s.number, field: 'environment' }));
    if (s.deny_local_logon && !denyLogonAllowed(c, s.number)) {
      out.push(issue('error', 'deny_local_logon_unsupported', { seat: s.number, field: 'deny_local_logon' }));
    }
  }
  if (!windowsUpdateValid(c.settings.tweaks.windows_update)) {
    out.push(issue('error', 'invalid_windows_update_policy', { field: 'settings.tweaks.windows_update' }));
  }
  const rules = c.settings.apps?.object_rules ?? [];
  if (rules.length > MAX_OBJECT_RULES || rules.some((r) => !objectRuleValid(r))) {
    out.push(issue('error', 'invalid_object_rule', { field: 'settings.apps.object_rules' }));
  }
  return out;
}

/** Device kinds that each install class covers (for the "no seat uses it" hint). */
const CLASS_KINDS: Partial<Record<DeviceInstallClass, DeviceKind[]>> = {
  printer: ['printer'],
  camera: ['webcam'],
  image: ['webcam'],
  smart_card_reader: ['smart_card_reader'],
  cdrom: ['storage'],
  ports: ['serial_port']
};

/** Install classes no seat has a device of (suggested for the deny policy). A class whose
 *  devices the manager cannot see (WPD, modem) counts as unused. */
export function unusedInstallClasses(c: Config, views: DeviceView[]): DeviceInstallClass[] {
  const assigned = new Set(c.devices.filter((d) => d.mask !== 0).map((d) => d.kind));
  const present = (k: DeviceKind) => views.some((v) => v.device.kind === k && v.device.present && v.effective !== 0);
  return INSTALL_CLASSES.filter((cls) => {
    const kinds = CLASS_KINDS[cls] ?? [];
    if (cls === 'cdrom') {
      return !views.some((v) => v.device.details.type === 'storage' && v.device.details.optical && v.effective !== 0);
    }
    return !kinds.some((k) => assigned.has(k) || present(k));
  });
}

// ---------------------------------------------------------------- USB dock wizard

/** One multi-seat USB dock: a USB hub with a monitor plus input or audio behind it. */
export interface UsbDock {
  /** Assignable key of the whole dock (hub container, else the hub itself). */
  key: DeviceKey;
  /** The dock's USB hub device. */
  hub: Device;
  /** Serial number or instance id of the hub (shown to tell docks apart). */
  label: string;
  monitors: Device[];
  inputs: Device[];
  audio: Device[];
  others: Device[];
  /** The seat that owns all of the dock's monitors now, if any. */
  seat: SeatNumber | null;
}

/** Devices behind `hub` (parent chain) or in its container. */
function behind(views: DeviceView[], hub: Device): DeviceView[] {
  const byKey = new Map(views.map((v) => [v.device.key, v.device]));
  const under = (d: Device): boolean => {
    let p = d.parent_hub;
    for (let i = 0; p && i < 16; i++) {
      if (p === hub.key) return true;
      p = byKey.get(p)?.parent_hub;
    }
    return false;
  };
  return views.filter(
    (v) => v.device.key !== hub.key && v.device.present && (under(v.device) || (!!hub.container_id && v.device.container_id === hub.container_id))
  );
}

/** Multi-seat USB docks: present USB hubs that have a monitor and at least one keyboard,
 *  mouse or sound device behind them (same container or parent chain). Nested hubs of a
 *  dock are folded into the outermost one. Order: by hub key (stable between scans). */
export function usbDocks(views: DeviceView[]): UsbDock[] {
  const hubs = views.filter((v) => v.device.kind === 'usb_hub' && v.device.present).map((v) => v.device);
  const docks: UsbDock[] = [];
  for (const hub of hubs) {
    const devs = behind(views, hub);
    const monitors = devs.filter((v) => v.device.kind === 'monitor').map((v) => v.device);
    const inputs = devs.filter((v) => ['keyboard', 'mouse', 'hid'].includes(v.device.kind)).map((v) => v.device);
    const audio = devs.filter((v) => v.device.kind === 'audio_render' || v.device.kind === 'audio_capture').map((v) => v.device);
    if (monitors.length === 0 || inputs.length + audio.length === 0) continue;
    const others = devs.filter((v) => !monitors.includes(v.device) && !inputs.includes(v.device) && !audio.includes(v.device)).map((v) => v.device);
    const masks = views.filter((v) => monitors.includes(v.device)).map((v) => v.effective);
    const single = masks.length > 0 && masks.every((m) => m === masks[0]) && masks[0] !== 0 && (masks[0] & (masks[0] - 1)) === 0 ? Math.log2(masks[0]) + 1 : null;
    const hubView = views.find((v) => v.device.key === hub.key);
    docks.push({
      key: hubView?.container_key ?? hub.key,
      hub,
      label: hub.serial || hub.instance_id || hub.name,
      monitors,
      inputs,
      audio,
      others,
      seat: single
    });
  }
  // Fold nested docks: drop a dock whose hub sits behind another dock's hub.
  const nested = (d: UsbDock) => docks.some((o) => o !== d && behind(views, o.hub).some((v) => v.device.key === d.hub.key));
  return docks.filter((d) => !nested(d)).sort((a, b) => (a.hub.key < b.hub.key ? -1 : a.hub.key > b.hub.key ? 1 : 0));
}

/** Pre-selected seat per dock: keep the seat that already owns its monitors, then fill
 *  seats 2.. without a display in order; `null` = needs a new seat. */
export function proposeDockSeats(docks: UsbDock[], seatsWithDisplay: SeatNumber[], seats: SeatNumber[]): (SeatNumber | null)[] {
  const used = new Set<SeatNumber>(docks.map((d) => d.seat).filter((s): s is SeatNumber => s !== null));
  const free = seats.filter((s) => s !== 1 && !seatsWithDisplay.includes(s) && !used.has(s));
  return docks.map((d) => d.seat ?? free.shift() ?? null);
}

/** The dock count is stable when the last `n` scans agree (the reference waits too). */
export function dockCountStable(history: number[], n = 2): boolean {
  if (history.length < n) return false;
  const tail = history.slice(-n);
  return tail.every((x) => x === tail[0]);
}
