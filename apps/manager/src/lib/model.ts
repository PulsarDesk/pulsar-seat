// SPDX-License-Identifier: GPL-3.0-only
// Pure config-editing logic for the manager (spec 10 §3, §4, §7). Every function returns a
// new Config and never mutates its input, so the store can keep the saved copy for the
// "unsaved changes" check. The service stays authoritative: it re-validates on
// `validate_config` / `apply_config`; the checks here only give instant feedback.

import * as M from './mask.ts';
import {
  DEFAULT_LOOPBACK_LISTENER_PORT,
  EXPERIMENTAL_WARNING_VERSION,
  MAX_SEATS,
  SHARED_MASK,
  MAX_IP_PROGRAMS,
  MAX_PAD_IGNORE_DEVICES,
  DEFAULT_SEAT_REDIRECT,
  type AssignmentOptions,
  type Config,
  type Device,
  type IpBinding,
  type IpProgram,
  type LogSettings,
  type OutputSettings,
  type DeviceAssignment,
  type DeviceKey,
  type DeviceClassTarget,
  type DeviceKind,
  type DeviceView,
  type Issue,
  type KioskApp,
  type KioskShell,
  type KioskShellApp,
  type GpuPreference,
  type LocalSeat,
  MAX_KIOSK_APPS,
  type MaskSource,
  type SeatAccount,
  type SeatConfig,
  type SeatMask,
  type SeatNumber,
  type SeatRedirect
} from './types.ts';

export type SharePolicy = 'exclusive' | 'shareable' | 'never_gated';

/** Mirror of `DeviceKind::policy`. */
export function policyOf(kind: DeviceKind): SharePolicy {
  switch (kind) {
    case 'monitor':
    case 'keyboard':
    case 'mouse':
    case 'hid':
      return 'exclusive';
    case 'network':
      return 'never_gated';
    default:
      return 'shareable';
  }
}

/** Mirror of `DeviceKind::default_mask`. */
export function defaultMaskOf(kind: DeviceKind): SeatMask {
  return policyOf(kind) === 'exclusive' ? 1 : SHARED_MASK;
}

/** Category order on the canvas (spec 10 §3.1). */
export const KIND_ORDER: DeviceKind[] = [
  'monitor',
  'keyboard',
  'mouse',
  'hid',
  'audio_render',
  'audio_capture',
  'webcam',
  'smart_card_reader',
  'usb_hub',
  'storage',
  'printer',
  'serial_port',
  'other',
  'network'
];

export function clone<T>(v: T): T {
  return JSON.parse(JSON.stringify(v)) as T;
}

export function seatNumbers(c: Config): SeatNumber[] {
  return c.seats.map((s) => s.number).sort((a, b) => a - b);
}

export function seatCount(c: Config): number {
  return Math.max(1, ...c.seats.map((s) => s.number));
}

export function newSeat(number: SeatNumber): SeatConfig {
  return {
    number,
    name: String(number),
    autostart: true,
    account: { kind: 'logon_screen' },
    cpu_sets: [],
    cpu_hard_limit: false,
    wallpaper: false,
    kind: 'local',
    local: {}
  };
}

/** The local part of a seat to read (empty for a remote seat). */
export function localOf(s: SeatConfig | undefined): LocalSeat {
  return (s && (s.kind ?? 'local') === 'local' ? s.local : undefined) ?? {};
}

/** The local part of a seat to edit (created when missing); `null` for a remote seat. */
export function localMut(s: SeatConfig): LocalSeat | null {
  if ((s.kind ?? 'local') !== 'local') return null;
  s.kind = 'local';
  s.local ??= {};
  return s.local;
}

/** Sort seats, keep seat 1 present and always starting, sort devices (like `normalize`). */
export function normalize(c: Config): Config {
  const n = clone(c);
  n.seats.sort((a, b) => a.number - b.number);
  if (n.seats[0]?.number !== 1) n.seats.unshift(newSeat(1));
  n.seats[0].autostart = true;
  n.devices.sort((a, b) => (a.key < b.key ? -1 : a.key > b.key ? 1 : 0));
  // Older services send no `updates` (added 2026-11-13): opt-in stays off.
  if (n.settings && !n.settings.updates) n.settings.updates = { check: false, weekly: false };
  // Added 2026-11-13 (wizard-manager); older services omit them.
  if (n.settings && !n.settings.log) n.settings.log = { info: true, debug: false, trace: false };
  if (n.settings && !n.settings.outputs) n.settings.outputs = { force_assign_at_start: false, allow_clone: false };
  if (n.settings && !n.settings.ip_programs) n.settings.ip_programs = [];
  if (n.settings?.backends && !n.settings.backends.loopback_listener) {
    n.settings.backends.loopback_listener = { enabled: true, port: DEFAULT_LOOPBACK_LISTENER_PORT, extra_accounts: [] };
  }
  return n;
}

// ---------------------------------------------------------------- loopback listener

/** The default RDP-Tcp port; the second listener must use another one. */
export const RDP_TCP_DEFAULT_PORT = 3389;

/** Port the second listener may use (the service also refuses the live RDP-Tcp port). */
export function loopbackPortValid(p: number): boolean {
  return Number.isInteger(p) && p >= 1024 && p <= 65535 && p !== RDP_TCP_DEFAULT_PORT;
}

/** Same rule as the service's `parse_account`: `user` or `DOMAIN\user`, no `@`. */
export function accountEntryValid(s: string): boolean {
  const t = s.trim();
  const i = t.indexOf('\\');
  const [d, u] = i < 0 ? ['.', t] : [t.slice(0, i).trim(), t.slice(i + 1).trim()];
  return d.length > 0 && u.length > 0 && !u.includes('\\') && !u.includes('@');
}

/**
 * Local seats that are RDP sessions (seat 1 is the console under `seat_one_console`) and
 * have no account: they sign in at the logon screen through the second listener.
 */
export function logonScreenRdpSeats(c: Config): number[] {
  const topo = c.settings.backends.host_topology;
  return c.seats
    .filter((s) => (s.kind ?? 'local') === 'local' && !(s.number === 1 && topo !== 'dedicated_host') && s.account.kind === 'logon_screen')
    .map((s) => s.number);
}

/**
 * Accounts the second listener admits: the accounts of the RDP seats plus the valid extra
 * accounts, as `user` / `DOMAIN\user` (seat-service `loopback_plan::plan_for`).
 */
export function loopbackAllowedAccounts(c: Config): string[] {
  const topo = c.settings.backends.host_topology;
  const out: string[] = [];
  const push = (a: string) => {
    if (!out.some((x) => x.toLowerCase() === a.toLowerCase())) out.push(a);
  };
  for (const s of c.seats) {
    if (s.number === 1 && topo !== 'dedicated_host') continue;
    if (s.account.kind === 'local') push(s.account.user);
    else if (s.account.kind === 'domain') push(`${s.account.domain}\\${s.account.user}`);
  }
  for (const a of c.settings.backends.loopback_listener?.extra_accounts ?? []) {
    if (accountEntryValid(a)) push(a.trim());
  }
  return out;
}

/** seat-core `loopback_account_checks` (`rds_loopback` only): logon-screen seats and the second listener. */
export function loopbackAccountIssues(c: Config): Issue[] {
  const seats = logonScreenRdpSeats(c);
  if (c.settings.backends.session !== 'rds_loopback' || seats.length === 0) return [];
  const enabled = c.settings.backends.loopback_listener?.enabled ?? true;
  if (!enabled) {
    return seats.map((n) => issue('warning', 'logon_screen_needs_listener', { seat: n, field: 'settings.backends.loopback_listener.enabled' }));
  }
  if (loopbackAllowedAccounts(c).length > 0) return [];
  return seats.map((n) => issue('warning', 'loopback_no_accounts', { seat: n, field: 'settings.backends.loopback_listener.extra_accounts' }));
}

/** One account per line (or comma); trimmed, empty lines and duplicates dropped. */
export function parseAccountList(text: string): string[] {
  const out: string[] = [];
  for (const raw of text.split(/[\n,]/)) {
    const a = raw.trim();
    if (a && !out.some((x) => x.toLowerCase() === a.toLowerCase())) out.push(a);
  }
  return out;
}

// ---------------------------------------------------------------- seats

/** Lowest free seat number, or null at the seat limit. */
export function nextSeatNumber(c: Config): SeatNumber | null {
  const used = new Set(c.seats.map((s) => s.number));
  for (let n = 1; n <= MAX_SEATS; n++) if (!used.has(n)) return n;
  return null;
}

export function addSeat(c: Config, name?: string): { config: Config; seat: SeatNumber } | null {
  const n = nextSeatNumber(c);
  if (n === null) return null;
  const out = clone(c);
  const seat = newSeat(n);
  if (name && name.trim()) seat.name = name.trim();
  out.seats.push(seat);
  return { config: normalize(out), seat: n };
}

/** Remove a seat (never seat 1). Its devices go to the spare pool; shared stays shared. */
export function removeSeat(c: Config, n: SeatNumber): Config {
  if (n === 1) return c;
  const out = clone(c);
  out.seats = out.seats.filter((s) => s.number !== n);
  for (const d of out.devices) {
    if (M.isShared(d.mask)) continue;
    d.mask = M.remove(d.mask, n);
  }
  return normalize(out);
}

function editSeat(c: Config, n: SeatNumber, f: (s: SeatConfig) => void): Config {
  const out = clone(c);
  const s = out.seats.find((x) => x.number === n);
  if (s) f(s);
  return normalize(out);
}

export function renameSeat(c: Config, n: SeatNumber, name: string): Config {
  return editSeat(c, n, (s) => (s.name = name.trim()));
}

export function setAutostart(c: Config, n: SeatNumber, on: boolean): Config {
  return editSeat(c, n, (s) => (s.autostart = n === 1 ? true : on));
}

export function setAccount(c: Config, n: SeatNumber, account: SeatAccount): Config {
  return editSeat(c, n, (s) => (s.account = account));
}

export function setWallpaper(c: Config, n: SeatNumber, on: boolean): Config {
  return editSeat(c, n, (s) => (s.wallpaper = on));
}

/** Most programs of a seat's GPU preference (`seat_core::config::MAX_GPU_PREF_APPS`). */
export const MAX_GPU_PREF_APPS = 64;

/** A program path the service accepts for a GPU preference: absolute, `.exe`, at most
 *  260 characters, no control characters, no outer blanks (seat-core
 *  `gpu_preference_checks`). */
export function gpuPrefAppValid(p: string): boolean {
  return p.length > 0 && [...p].length <= 260 && p.trim() === p && !/[\u0000-\u001f]/.test(p) && isAbsoluteWindowsPath(p) && p.toLowerCase().endsWith('.exe');
}

/** Per-seat GPU preference (off with no programs = field removed, as the service writes it). */
export function setGpuPreference(c: Config, n: SeatNumber, pref: GpuPreference): Config {
  return editSeat(c, n, (s) => {
    const l = localMut(s);
    if (!l) return;
    const apps = pref.apps?.filter((a) => a.trim() !== '') ?? [];
    if (pref.mode === 'off' && apps.length === 0) delete l.gpu_preference;
    else l.gpu_preference = apps.length ? { mode: pref.mode, apps } : { mode: pref.mode };
  });
}

/** Set or clear the HDR / VRR option of a monitor (seats 2..N, spec 40 §6.6). */
export function setDisplayOption(c: Config, d: Device, key: 'hdr' | 'vrr', on: boolean, currentMask: SeatMask): Config {
  let out = assignmentOf(c, d.key) ? clone(c) : setDeviceMask(c, d, currentMask);
  out = clone(out);
  const a = out.devices.find((x) => x.key === d.key)!;
  a.options = { ...(a.options ?? {}), [key]: on };
  if (!a.options[key]) delete a.options[key];
  if (Object.keys(a.options).length === 0) delete a.options;
  return out;
}

/** Seat-owned virtual pads of a local seat (off = field removed, as the service writes it). */
export function setVirtualPads(c: Config, n: SeatNumber, on: boolean): Config {
  return editSeat(c, n, (s) => {
    const l = localMut(s);
    if (!l) return;
    if (on) l.virtual_pads = true;
    else delete l.virtual_pads;
  });
}

/** `0xVVVV/0xPPPP` (also without `0x`, any case), as `seat_core::validate::is_vid_pid`. */
export function isVidPid(s: string): boolean {
  const parts = s.trim().split('/');
  if (parts.length !== 2) return false;
  return parts.every((p) => /^(0x)?[0-9a-f]{4}$/i.test(p.trim()));
}

/** Parses a comma / space separated id list as typed in the seat table. */
export function parsePadIgnoreList(text: string): string[] {
  return text
    .split(/[\s,;]+/)
    .map((x) => x.trim())
    .filter((x) => x.length > 0);
}

/** SDL ignore list of a seat (empty = field removed, as the service writes it). */
export function setPadIgnoreDevices(c: Config, n: SeatNumber, ids: string[]): Config {
  return editSeat(c, n, (s) => {
    if (ids.length) s.pad_ignore_devices = ids;
    else delete s.pad_ignore_devices;
  });
}

/** Physical pad scoping (`settings.input.pad_device_scope`). */
export function setPadDeviceScope(c: Config, on: boolean): Config {
  const next = structuredClone(c);
  next.settings.input.pad_device_scope = on;
  return next;
}

export function setCpuSets(c: Config, n: SeatNumber, ids: number[], hard: boolean): Config {
  return editSeat(c, n, (s) => {
    s.cpu_sets = [...ids].sort((a, b) => a - b);
    s.cpu_hard_limit = hard;
  });
}

/** Validation of the seat-name dialog (spec 10 §7.2; 1..=64 chars, no control chars). */
export function seatNameError(name: string): 'empty' | 'too_long' | 'control' | null {
  const t = name.trim();
  if ([...t].length === 0) return 'empty';
  if ([...t].length > 64) return 'too_long';
  // eslint-disable-next-line no-control-regex
  if (/[\u0000-\u001f\u007f]/.test(name)) return 'control';
  return null;
}

/** Mirror of the account check in `validate`. */
export function accountError(a: SeatAccount): 'user' | 'domain' | null {
  const bad = (x: string) => x.trim() === '' || x.length > 256 || /[\\/"@]/.test(x);
  if (a.kind === 'logon_screen') return null;
  if (bad(a.user)) return 'user';
  if (a.kind === 'domain' && bad(a.domain)) return 'domain';
  return null;
}

// ---------------------------------------------------------------- devices

export function assignmentOf(c: Config, key: DeviceKey): DeviceAssignment | undefined {
  return c.devices.find((d) => d.key === key);
}

/** Exclusive kinds keep at most one seat: the lowest seat of the mask wins. */
export function coerceMask(kind: DeviceKind, mask: SeatMask): SeatMask {
  const p = policyOf(kind);
  if (p === 'never_gated') return SHARED_MASK;
  if (p === 'shareable') return M.u32(mask);
  if (M.isShared(mask)) return 1;
  const seats = M.seatsOf(mask);
  return seats.length === 0 ? 0 : M.bit(seats[0]);
}

/** Give a device an explicit mask (creates or updates its assignment record). */
export function setDeviceMask(c: Config, d: Device, mask: SeatMask): Config {
  const out = clone(c);
  const m = coerceMask(d.kind, mask);
  const existing = out.devices.find((x) => x.key === d.key);
  if (existing) {
    existing.mask = m;
    existing.name = d.name;
    if (d.label) existing.label = d.label;
  } else {
    const a: DeviceAssignment = { key: d.key, kind: d.kind, mask: m, name: d.name };
    if (d.label) a.label = d.label;
    if (d.hardware_id) a.hardware_id = d.hardware_id;
    if (d.serial) a.serial = d.serial;
    if (d.container_id) a.container_id = d.container_id;
    out.devices.push(a);
  }
  return normalize(out);
}

/** Assignment of a port (`port;loc=…`, kind `usb_hub`) or a whole device (`cont;id=…`,
 *  kind `other`), spec 70 §5.1. Mask 0 (spare) removes it again. */
export function setKeyMask(c: Config, key: DeviceKey, kind: DeviceKind, mask: SeatMask, name: string): Config {
  if (mask === 0) return clearDeviceMask(c, key);
  const out = clone(c);
  const existing = out.devices.find((x) => x.key === key);
  if (existing) existing.mask = mask;
  else out.devices.push({ key, kind, mask, name });
  return normalize(out);
}

/** Forget the explicit assignment: the device inherits from its hub or takes its default. */
export function clearDeviceMask(c: Config, key: DeviceKey): Config {
  const out = clone(c);
  out.devices = out.devices.filter((d) => d.key !== key);
  return out;
}

export function setSoftwareCursor(c: Config, d: Device, on: boolean, currentMask: SeatMask): Config {
  let out = assignmentOf(c, d.key) ? clone(c) : setDeviceMask(c, d, currentMask);
  out = clone(out);
  const a = out.devices.find((x) => x.key === d.key)!;
  a.options = { ...(a.options ?? {}), software_cursor: on };
  if (!a.options.software_cursor) delete a.options.software_cursor;
  if (Object.keys(a.options).length === 0) delete a.options;
  return out;
}

/** Set or clear one assignment option; the assignment is created with the current mask. */
function setOption<K extends keyof AssignmentOptions>(c: Config, d: Device, currentMask: SeatMask, k: K, v: AssignmentOptions[K] | undefined): Config {
  let out = assignmentOf(c, d.key) ? clone(c) : setDeviceMask(c, d, currentMask);
  out = clone(out);
  const a = out.devices.find((x) => x.key === d.key)!;
  const o: AssignmentOptions = { ...(a.options ?? {}) };
  const empty = v === undefined || v === false || (Array.isArray(v) && v.length === 0);
  if (empty) delete o[k];
  else o[k] = v;
  if (Object.keys(o).length === 0) delete a.options;
  else a.options = o;
  return out;
}

/** Custom picture (spec 10 §7.9): `builtin:<id>`, `user:<file>`, or undefined = default. */
export function setPicture(c: Config, d: Device, picture: string | undefined, currentMask: SeatMask): Config {
  return setOption(c, d, currentMask, 'picture', picture);
}

/** Global "clear all custom pictures" (spec 10 §3.1). */
export function clearAllPictures(c: Config): Config {
  const out = clone(c);
  for (const a of out.devices) {
    if (!a.options?.picture) continue;
    delete a.options.picture;
    if (Object.keys(a.options).length === 0) delete a.options;
  }
  return out;
}

/** Outputs feeding a monitor (spec 10 §7.5); empty = automatic. */
export function setMonitorOutputs(c: Config, d: Device, targets: number[], currentMask: SeatMask): Config {
  const uniq = [...new Set(targets)].sort((a, b) => a - b);
  return setOption(c, d, currentMask, 'outputs', uniq);
}

export function setOutputSettings(c: Config, s: OutputSettings): Config {
  const out = clone(c);
  out.settings.outputs = { ...s };
  return out;
}

export function setLogLevels(c: Config, l: LogSettings): Config {
  const out = clone(c);
  out.settings.log = { ...l };
  return out;
}

/** Dotted IPv4 that seat-core accepts (not 0.0.0.0, broadcast or multicast). */
export function isUsableIpv4(a: string): boolean {
  const m = /^(\d{1,3})\.(\d{1,3})\.(\d{1,3})\.(\d{1,3})$/.exec(a.trim());
  if (!m) return false;
  const b = m.slice(1).map(Number);
  if (b.some((x, i) => x > 255 || (m[i + 1].length > 1 && m[i + 1].startsWith('0')))) return false;
  if (b.every((x) => x === 0) || b.every((x) => x === 255)) return false;
  // Multicast (224-239) and class E (240-255) are never a source address (seat-core).
  return b[0] < 224;
}

/** 127.0.0.0/8: refused as a seat address (the source-IP driver would cut the seat off
 *  the LAN). */
export function isLoopbackIpv4(a: string): boolean {
  return /^127\.\d{1,3}\.\d{1,3}\.\d{1,3}$/.test(a.trim());
}

/** 169.254.0.0/16: assigned by Windows when DHCP fails, may change. */
export function isLinkLocalIpv4(a: string): boolean {
  return /^169\.254\.\d{1,3}\.\d{1,3}$/.test(a.trim());
}

/** Per-seat source address (spec 10 §7.4); `null` = not assigned. */
export function setIpBinding(c: Config, n: SeatNumber, binding: IpBinding | null): Config {
  const out = clone(c);
  const s = out.seats.find((x) => x.number === n);
  if (!s) return c;
  if (binding) s.ip_binding = { address: binding.address.trim(), rule: binding.rule };
  else delete s.ip_binding;
  return out;
}

/** Absolute Windows path (`C:\…` or `\\server\share\…`): the service never resolves a bare
 *  program name through a search path. */
export function isAbsoluteWindowsPath(p: string): boolean {
  const v = p.trim();
  return /^[A-Za-z]:[\\/]/.test(v) || /^\\\\[^\\]+\\[^\\]+/.test(v);
}

/** Application User Model ID of a packaged app (`Family!App`), same rule as seat-core
 *  `validate::is_app_user_model_id`. */
export function isAppUserModelId(s: string): boolean {
  const v = s.trim();
  return v.length <= 160 && /^[A-Za-z0-9._-]+![A-Za-z0-9._-]+$/.test(v);
}

/** Splits an argument line the way the manager shows it back ({@link formatArgs}):
 *  whitespace separates, double quotes group. */
export function parseArgs(line: string): string[] {
  const out: string[] = [];
  let cur = '';
  let quoted = false;
  let any = false;
  for (const ch of line) {
    if (ch === '"') {
      quoted = !quoted;
      any = true;
    } else if (!quoted && /\s/.test(ch)) {
      if (any) out.push(cur);
      cur = '';
      any = false;
    } else {
      cur += ch;
      any = true;
    }
  }
  if (any) out.push(cur);
  return out;
}

export function formatArgs(args: string[]): string {
  return args.map((a) => (a === '' || /[\s"]/.test(a) ? `"${a.replace(/"/g, '')}"` : a)).join(' ');
}

/** Startup programs of a seat (spec 80 §11.2); empty commands are dropped. */
export function setStartupApps(c: Config, n: SeatNumber, apps: KioskApp[]): Config {
  const out = clone(c);
  const s = out.seats.find((x) => x.number === n);
  if (!s) return c;
  const list = apps.map((a) => ({ command: a.command.trim(), args: a.args })).filter((a) => a.command !== '');
  if (list.length) s.startup_apps = list;
  else delete s.startup_apps;
  return out;
}

/** The seat's redirection policy, filled with the defaults for missing fields. */
export function seatRedirect(c: Config, n: SeatNumber): SeatRedirect {
  const r = localOf(c.seats.find((x) => x.number === n)).redirect;
  return {
    clipboard: { ...DEFAULT_SEAT_REDIRECT.clipboard, ...(r?.clipboard ?? {}) },
    drives: r?.drives ?? DEFAULT_SEAT_REDIRECT.drives,
    smartcards: r?.smartcards ?? DEFAULT_SEAT_REDIRECT.smartcards,
    printers: r?.printers ?? 'off'
  };
}

/** Whether a redirection policy turns anything on. */
export function redirectOn(r: SeatRedirect): boolean {
  return r.clipboard.direction !== 'off' || r.drives !== 'off' || r.smartcards || (r.printers ?? 'off') !== 'off';
}

/** Set a seat's redirection policy; the default (all off) is not written (like the service).
 *  Seat 1 runs on the console and never redirects. */
export function setRedirect(c: Config, n: SeatNumber, r: SeatRedirect): Config {
  const out = clone(c);
  const s = out.seats.find((x) => x.number === n);
  const l = s ? localMut(s) : null;
  if (!l) return c;
  const full: SeatRedirect = { clipboard: { ...r.clipboard }, drives: r.drives, smartcards: r.smartcards, printers: r.printers ?? 'off' };
  if (n === 1 || JSON.stringify(full) === JSON.stringify(DEFAULT_SEAT_REDIRECT)) delete l.redirect;
  else l.redirect = full;
  return out;
}

/** Kiosk shell of a seat (spec 35 §5.1 U3); `null` turns the seat back into a normal seat.
 *  Rows without a command are dropped, titles default to the program name. */
export function setKioskShell(c: Config, n: SeatNumber, shell: KioskShell | null): Config {
  const out = clone(c);
  const s = out.seats.find((x) => x.number === n);
  const l = s ? localMut(s) : null;
  if (!l) return c;
  if (!shell) {
    delete l.kiosk_shell;
    return out;
  }
  const apps = shell.apps
    .map((a) => {
      const command = a.command.trim();
      const appId = a.app_id?.trim() ?? '';
      const fallback = appId ? (appId.split('!').pop() ?? appId) : command;
      const base = command ? (command.split(/[\\/]/).pop()?.replace(/\.exe$/i, '') ?? command) : fallback;
      const app: KioskShellApp = { title: a.title.trim() || base, command, autostart: a.autostart, monitor: Math.max(0, Math.min(15, Math.trunc(a.monitor) || 0)) };
      if (a.args?.length) app.args = a.args;
      if (appId) app.app_id = appId;
      if (a.fullscreen) app.fullscreen = true;
      return app;
    })
    .filter((a) => a.command !== '' || a.app_id !== undefined)
    .slice(0, MAX_KIOSK_APPS);
  const idle = Math.max(0, Math.min(1440, Math.trunc(shell.idle_reset_minutes) || 0));
  l.kiosk_shell = { apps, idle_reset_minutes: idle };
  return out;
}

/** Seat 1 stays the normal desktop next to kiosk seats (default true). */
export function kioskSeatOneDesktop(c: Config): boolean {
  return c.settings.kiosk?.seat1_desktop ?? true;
}

export function setKioskSeatOneDesktop(c: Config, on: boolean): Config {
  const out = clone(c);
  out.settings.kiosk = { ...out.settings.kiosk, seat1_desktop: on };
  return out;
}

/** Kiosk seats 2+ run as their own Windows user (default; 2026-11-17). */
export function kioskPerSeatAccounts(c: Config): boolean {
  return (c.settings.kiosk?.accounts ?? 'per_seat') === 'per_seat';
}

export function setKioskPerSeatAccounts(c: Config, on: boolean): Config {
  const out = clone(c);
  out.settings.kiosk = { seat1_desktop: kioskSeatOneDesktop(c), ...out.settings.kiosk, accounts: on ? 'per_seat' : 'shared' };
  return out;
}

/** Whose Windows user a kiosk seat runs as (`null` = not a kiosk shell seat): seat 1 is the
 *  console user, `shared` too; else the seat's account when a password is stored for it,
 *  else Pulsar's own `PulsarKiosk<NN>` (seat-session `kiosk_account::identity_for`). */
export function kioskAccountOf(c: Config, n: SeatNumber, passwordSet: boolean): { kind: 'console' } | { kind: 'seat'; user: string } | { kind: 'auto'; user: string } | null {
  const s = c.seats.find((x) => x.number === n);
  if (!s || !localOf(s).kiosk_shell) return null;
  if (n === 1 || !kioskPerSeatAccounts(c)) return { kind: 'console' };
  const a = s.account;
  if (passwordSet && a && a.kind !== 'logon_screen' && a.user.trim() !== '' && !/^PulsarKiosk\d\d$/i.test(a.user)) {
    return { kind: 'seat', user: a.kind === 'domain' ? `${a.domain}\\${a.user}` : a.user };
  }
  return { kind: 'auto', user: `PulsarKiosk${String(n).padStart(2, '0')}` };
}

/** Audio of a kiosk seat (round 4 kiosk-audio). The shell gives each of the seat's apps
 *  the seat's own endpoints as their per-app default (undocumented Windows API, from
 *  Windows 10 1803; seat-media `kiosk_audio`). Mirrors the shell's pick: per flow the first
 *  active endpoint whose effective mask is exactly this seat, else the first one. */
export interface KioskAudioView {
  state: 'routed' | 'no_endpoint' | 'unsupported';
  render: string | null;
  capture: string | null;
  /** Other kiosk seats that list one of this seat's programs (Windows keeps the per-app
   *  endpoint per program, so they share it; HYPOTHESIS). */
  sharedWith: SeatNumber[];
}

/** `null` for a seat that does not run a kiosk shell. `sessionNotes` = the `desktops`
 *  backend's notes (`kiosk_audio_shared` = Windows too old). */
export function kioskAudio(c: Config, n: SeatNumber, devices: DeviceView[], sessionNotes: string[]): KioskAudioView | null {
  const seatOf = (x: SeatConfig) => (x.number === 1 && kioskSeatOneDesktop(c) ? undefined : localOf(x).kiosk_shell);
  const s = c.seats.find((x) => x.number === n);
  const shell = s ? seatOf(s) : undefined;
  if (!shell) return null;
  const pick = (flow: 'render' | 'capture'): string | null => {
    const mine = devices.filter((v) => v.device.present && v.device.details.type === 'audio' && v.device.details.flow === flow && M.single(v.effective) === n);
    const best = mine.find((v) => v.device.details.type === 'audio' && v.device.details.state === 'active') ?? mine[0];
    return best ? best.device.name : null;
  };
  const render = pick('render');
  const capture = pick('capture');
  const mineCmds = new Set(shell.apps.map((a) => a.command.trim().toLowerCase()));
  const sharedWith = c.seats
    .filter((x) => x.number !== n)
    .filter((x) => seatOf(x)?.apps.some((a) => mineCmds.has(a.command.trim().toLowerCase())))
    .map((x) => x.number);
  const state = sessionNotes.includes('kiosk_audio_shared') ? 'unsupported' : render || capture ? 'routed' : 'no_endpoint';
  return { state, render, capture, sharedWith };
}

/** Classes for the class-level default descriptors; empty = the service default. */
export function setClassDefaultClasses(c: Config, classes: DeviceClassTarget[]): Config {
  const out = clone(c);
  const uniq = [...new Set(classes)];
  if (uniq.length) out.settings.devices.class_default_classes = uniq;
  else delete out.settings.devices.class_default_classes;
  return out;
}

export function setIpPrograms(c: Config, list: IpProgram[]): Config {
  const out = clone(c);
  out.settings.ip_programs = list.map((p) => ({ title: p.title.trim(), path: p.path.trim(), enabled: p.enabled }));
  return out;
}

export type DropTarget = SeatNumber | 'system';

/**
 * Drag and drop (spec 10 §3.4): dropping on the system container makes the device spare;
 * dropping on a seat moves it there. Shareable devices move from the source seat to the
 * target and keep their other seats; a shared device dropped on a seat becomes that seat's.
 */
export function moveDevice(c: Config, d: Device, current: SeatMask, from: DropTarget, to: DropTarget): Config {
  if (to === 'system') return setDeviceMask(c, d, 0);
  if (policyOf(d.kind) !== 'shareable' || M.isShared(current) || from === 'system') {
    return setDeviceMask(c, d, M.bit(to));
  }
  return setDeviceMask(c, d, M.insert(M.remove(current, from), to));
}

export interface Effective {
  mask: SeatMask;
  source: MaskSource;
}

/** Mirror of `seat_core::assign::effective_masks`, so the canvas follows unsaved edits. */
export function effectiveMasks(c: Config, inventory: Device[]): Map<DeviceKey, Effective> {
  const explicit = new Map(c.devices.map((d) => [d.key, M.u32(d.mask)]));
  const byKey = new Map(inventory.map((d) => [d.key, d]));
  const out = new Map<DeviceKey, Effective>();
  for (const d of inventory) {
    out.set(d.key, resolve(d, explicit, byKey));
  }
  return out;
}

function resolve(d: Device, explicit: Map<DeviceKey, SeatMask>, byKey: Map<DeviceKey, Device>): Effective {
  if (policyOf(d.kind) === 'never_gated') return { mask: SHARED_MASK, source: { type: 'default' } };
  const own = explicit.get(d.key);
  if (own !== undefined) return { mask: own, source: { type: 'explicit' } };
  const exclusive = policyOf(d.kind) === 'exclusive';
  const fallback: Effective = { mask: defaultMaskOf(d.kind), source: { type: 'default' } };
  // A non-spare assignment decides; an exclusive kind takes it only for one seat, else the
  // walk stops at the kind default (the nearest assignment wins).
  const take = (k: DeviceKey): Effective | 'stop' | undefined => {
    const m = explicit.get(k);
    if (m === undefined || M.isSpare(m)) return undefined;
    return !exclusive || M.single(m) !== null ? { mask: m, source: { type: 'inherited', hub: k } } : 'stop';
  };
  // 1. The whole physical device (container).
  const ck = d.container_id ? containerKey(d.container_id) : null;
  if (ck) {
    const r = take(ck);
    if (r) return r === 'stop' ? fallback : r;
  }
  // 2. Nearest first: own port, its hub, that hub's port, the next hub, ...
  let node: DeviceKey = d.key;
  let hub = d.parent_hub;
  for (let guard = 0; guard <= 32; guard++) {
    const loc = keyLocation(node);
    const port = loc ? portOfLocation(loc) : null;
    if (port) {
      const r = take(portKey(port));
      if (r) return r === 'stop' ? fallback : r;
    }
    if (hub === undefined) break;
    const r = take(hub);
    if (r) return r === 'stop' ? fallback : r;
    node = hub;
    hub = byKey.get(hub)?.parent_hub;
  }
  return fallback;
}

// Mirrors of seat_core::pnp key helpers (port / container assignment keys, spec 70 §5.1).
const SYSTEM_CONTAINER_ID = '00000000-0000-0000-ffff-ffffffffffff';

/** Port path of a location path: up to and including its last `USB(n)` segment. */
export function portOfLocation(location: string): string | null {
  const segs = location.split('#');
  let last = -1;
  segs.forEach((s, i) => {
    if (s.startsWith('USB(') && s.endsWith(')')) last = i;
  });
  return last < 0 ? null : segs.slice(0, last + 1).join('#');
}

export function portKey(portPath: string): DeviceKey {
  // eslint-disable-next-line no-control-regex
  return `port;loc=${portPath.replace(/[\u0000-\u001f\u007f-\u009f]/g, '').replace(/;/g, '_')}`;
}

export function containerKey(containerId: string): DeviceKey | null {
  const c = containerId.trim().replace(/^[{}]+|[{}]+$/g, '').toLowerCase();
  if (!c || c === SYSTEM_CONTAINER_ID || !/^[0-9a-f-]+$/.test(c)) return null;
  return `cont;id=${c}`;
}

export function keyLocation(key: DeviceKey): string | null {
  for (const p of key.split(';')) if (p.startsWith('loc=')) return p.slice(4);
  return null;
}

export const isPortKey = (k: DeviceKey) => k.startsWith('port;');
export const isContainerKey = (k: DeviceKey) => k.startsWith('cont;');

// ---------------------------------------------------------------- canvas

export interface ViewPrefs {
  sharedInSeats: boolean;
  showAllHubs: boolean;
  visibleSeats: SeatMask;
}

export interface CanvasItem {
  view: DeviceView;
  mask: SeatMask;
  source: MaskSource;
  shared: boolean;
}

export interface Canvas {
  system: CanvasItem[];
  seats: Map<SeatNumber, CanvasItem[]>;
}

function kindRank(k: DeviceKind): number {
  const i = KIND_ORDER.indexOf(k);
  return i < 0 ? KIND_ORDER.length : i;
}

/** Category first, then arrival: a re-plugged device goes to the end of its category. */
export function sortViews(views: DeviceView[]): DeviceView[] {
  return [...views].sort(
    (a, b) =>
      kindRank(a.device.kind) - kindRank(b.device.kind) ||
      a.arrived_ms - b.arrived_ms ||
      (a.device.key < b.device.key ? -1 : a.device.key > b.device.key ? 1 : 0)
  );
}

/**
 * Rows of a column-major device grid (spec 10 §3.1: devices are listed top to bottom, then
 * the next column). `width` = inner width of the container, `tile` = one tile with its gap.
 */
export function gridRows(count: number, width: number, tile: number): number {
  if (count <= 0) return 1;
  const cols = Math.max(1, Math.floor((width > 0 ? width : tile) / Math.max(1, tile)));
  return Math.max(1, Math.ceil(count / cols));
}

/** Hubs with nothing but (empty) hubs below them (spec 10 §4.5 `showAllUsbHubs`). */
export function emptyHubs(inventory: Device[]): Set<DeviceKey> {
  const children = new Map<DeviceKey, Device[]>();
  for (const d of inventory) {
    if (d.parent_hub) children.set(d.parent_hub, [...(children.get(d.parent_hub) ?? []), d]);
  }
  const memo = new Map<DeviceKey, boolean>();
  const isEmpty = (key: DeviceKey, depth: number): boolean => {
    if (memo.has(key)) return memo.get(key)!;
    if (depth > 32) return false;
    const kids = children.get(key) ?? [];
    const v = kids.every((k) => k.kind === 'usb_hub' && isEmpty(k.key, depth + 1));
    memo.set(key, v);
    return v;
  };
  const out = new Set<DeviceKey>();
  for (const d of inventory) if (d.kind === 'usb_hub' && isEmpty(d.key, 0)) out.add(d.key);
  return out;
}

/** Split the inventory into the system container (spare + shared) and seat containers. */
export function buildCanvas(c: Config, views: DeviceView[], prefs: ViewPrefs): Canvas {
  const inventory = views.map((v) => v.device);
  const eff = effectiveMasks(c, inventory);
  const hidden = prefs.showAllHubs ? new Set<DeviceKey>() : emptyHubs(inventory);
  const numbers = seatNumbers(c);
  const seats = new Map<SeatNumber, CanvasItem[]>(numbers.map((n) => [n, []]));
  const system: CanvasItem[] = [];
  for (const view of sortViews(views)) {
    if (hidden.has(view.device.key)) continue;
    const e = eff.get(view.device.key)!;
    const shared = M.isShared(e.mask);
    const item: CanvasItem = { view, mask: e.mask, source: e.source, shared };
    if (shared || M.isSpare(e.mask) || view.device.kind === 'network') {
      system.push(item);
      if (shared && prefs.sharedInSeats && view.device.kind !== 'network') {
        for (const n of numbers) seats.get(n)!.push(item);
      }
      continue;
    }
    let placed = false;
    for (const n of M.seatsOf(e.mask)) {
      const list = seats.get(n);
      if (list) {
        list.push(item);
        placed = true;
      }
    }
    // A mask naming only seats that do not exist (yet): show it in the pool with a sign.
    if (!placed) system.push(item);
  }
  return { system, seats };
}

export type SeatColour = 'grey' | 'red' | 'yellow' | 'green';
export type SeatReadiness = 'not_starting' | 'no_display' | 'no_keyboard' | 'no_mouse' | 'ready';

export interface SeatHealth {
  colour: SeatColour;
  readiness: SeatReadiness;
}

function isPointer(d: Device): boolean {
  if (d.kind === 'mouse') return true;
  // Touchpads, touch screens and pens (HID digitizer page 0x0D) also drive a pointer.
  return d.kind === 'hid' && d.details.type === 'input' && d.details.usage_page === 0x0d;
}

/** Seat container colour (spec 10 §3.3). */
export function seatHealth(c: Config, seat: SeatNumber, views: DeviceView[]): SeatHealth {
  const s = c.seats.find((x) => x.number === seat);
  if (!s || (!s.autostart && seat !== 1)) return { colour: 'grey', readiness: 'not_starting' };
  const inventory = views.map((v) => v.device);
  const eff = effectiveMasks(c, inventory);
  const mine = inventory.filter((d) => d.present && M.contains(eff.get(d.key)!.mask, seat));
  if (!mine.some((d) => d.kind === 'monitor')) return { colour: 'red', readiness: 'no_display' };
  if (!mine.some((d) => d.kind === 'keyboard')) return { colour: 'yellow', readiness: 'no_keyboard' };
  if (!mine.some(isPointer)) return { colour: 'yellow', readiness: 'no_mouse' };
  return { colour: 'green', readiness: 'ready' };
}

// ---------------------------------------------------------------- validation mirror

function issue(severity: Issue['severity'], code: string, extra: Partial<Issue> = {}): Issue {
  return { severity, code, ...extra };
}

/** Client-side subset of `seat_core::validate::validate` (same codes). */
export function localIssues(c: Config, inventory: Device[] | null): Issue[] {
  const out: Issue[] = [];
  if (c.seats.length > MAX_SEATS) out.push(issue('error', 'too_many_seats', { field: 'seats' }));
  const seen = new Set<number>();
  for (const s of c.seats) {
    if (seen.has(s.number)) out.push(issue('error', 'duplicate_seat', { seat: s.number }));
    seen.add(s.number);
    if (seatNameError(s.name)) out.push(issue('error', 'invalid_seat_name', { seat: s.number, field: 'name' }));
    if (accountError(s.account)) out.push(issue('error', 'invalid_account', { seat: s.number, field: 'account' }));
  }
  const count = seatCount(c);
  const keys = new Set<string>();
  for (const d of c.devices) {
    if (keys.has(d.key)) out.push(issue('error', 'duplicate_device', { device: d.key }));
    keys.add(d.key);
    if (M.beyond(d.mask, count)) out.push(issue('error', 'mask_beyond_seats', { device: d.key }));
    if (policyOf(d.kind) === 'exclusive' && (M.isShared(d.mask) || M.count(d.mask) > 1)) {
      out.push(issue('error', 'exclusive_on_several_seats', { device: d.key }));
    }
  }
  const t = c.settings.timings;
  if (t.seat_start_timeout_s < 30 || t.seat_start_timeout_s > 120) {
    out.push(issue('error', 'timeout_out_of_range', { field: 'settings.timings.seat_start_timeout_s' }));
  }
  if (t.between_seats_ms > 60_000 || t.relogon_delay_ms > 60_000 || t.start_delay_ms > 600_000) {
    out.push(issue('error', 'timeout_out_of_range', { field: 'settings.timings' }));
  }
  const idle = t.monitor_idle_off_s ?? 0;
  if (idle !== 0 && (idle < 60 || idle > 86_400)) {
    out.push(issue('error', 'timeout_out_of_range', { field: 'settings.timings.monitor_idle_off_s' }));
  }
  const sc = c.settings.input.switch_hotkey_scan;
  if (sc !== null && (sc === 0 || sc > 0x7f || sc === 0x1d)) {
    out.push(issue('error', 'invalid_hotkey', { field: 'settings.input.switch_hotkey_scan' }));
  }
  const b = c.settings.backends;
  if (b.session === 'termsrv_wrapper' && b.experimental_ack !== EXPERIMENTAL_WARNING_VERSION) {
    out.push(issue('error', 'experimental_not_acknowledged', { field: 'settings.backends.session' }));
  }
  const ll = b.loopback_listener;
  if (ll && b.session === 'rds_loopback' && ll.enabled && !loopbackPortValid(ll.port)) {
    out.push(issue('error', 'loopback_port_invalid', { field: 'settings.backends.loopback_listener.port' }));
  }
  // The tweak writes the HideFastUserSwitching policy, which turns fast_user_switch off
  // (seat-core `HideFsuWithFastUserSwitch`).
  if (b.session === 'fast_user_switch' && c.settings.tweaks.hide_fast_user_switching) {
    out.push(issue('error', 'hide_fsu_with_fast_user_switch', { field: 'settings.tweaks.hide_fast_user_switching' }));
  }
  if (ll && (ll.extra_accounts ?? []).some((a) => !accountEntryValid(a))) {
    out.push(issue('warning', 'loopback_account_invalid', { field: 'settings.backends.loopback_listener.extra_accounts' }));
  }
  out.push(...loopbackAccountIssues(c));
  // Fast user switching finds each seat's session by account: one account per seat
  // (seat-core `SameAccountFastUserSwitch`).
  const fus = b.session === 'fast_user_switch';
  if (c.settings.same_account === 'deny' || fus) {
    const users = new Set<string>();
    const code = c.settings.same_account === 'deny' ? 'same_account_on_seats' : 'same_account_fast_user_switch';
    for (const s of c.seats) {
      if (s.account.kind === 'logon_screen') continue;
      const d = s.account.kind === 'local' ? '.' : s.account.domain;
      const k = `${d.toLowerCase()}\\${s.account.user.toLowerCase()}`;
      if (users.has(k)) out.push(issue('error', code, { seat: s.number }));
      users.add(k);
    }
  }

  // SDL ignore lists (2026-11-17), same rules as seat-core.
  for (const s of c.seats) {
    const l = s.pad_ignore_devices ?? [];
    if (l.length > MAX_PAD_IGNORE_DEVICES || l.some((x) => !isVidPid(x))) {
      out.push(issue('error', 'invalid_pad_ignore_device', { seat: s.number, field: 'pad_ignore_devices' }));
    }
  }

  // Per-seat source addresses (spec 10 §7.4), same rules as seat-core.
  const ips = new Set<string>();
  for (const s of c.seats) {
    if (!s.ip_binding) continue;
    const a = s.ip_binding.address.trim();
    if (isUsableIpv4(a) && isLoopbackIpv4(a)) {
      out.push(issue('error', 'loopback_ip_address', { seat: s.number, field: 'ip_binding' }));
      continue;
    }
    if (!isUsableIpv4(a)) {
      out.push(issue('error', 'invalid_ip_address', { seat: s.number, field: 'ip_binding' }));
      continue;
    }
    if (isLinkLocalIpv4(a)) out.push(issue('warning', 'link_local_ip_address', { seat: s.number, field: 'ip_binding' }));
    if (ips.has(a)) out.push(issue('warning', 'duplicate_ip_address', { seat: s.number, field: 'ip_binding' }));
    else ips.add(a);
    if (s.local?.kiosk_shell) out.push(issue('warning', 'ip_binding_shared_session', { seat: s.number, field: 'ip_binding' }));
  }
  const progs = c.settings.ip_programs ?? [];
  if (progs.length > MAX_IP_PROGRAMS) out.push(issue('warning', 'too_many_ip_programs', { field: 'settings.ip_programs' }));
  if (progs.some((p) => !p.path.trim() || /[\u0000-\u001f]/.test(p.path))) {
    out.push(issue('error', 'invalid_ip_program', { field: 'settings.ip_programs' }));
  }
  // Kiosk shells (seat-core `kiosk_checks`).
  for (const s of c.seats) {
    const k = localOf(s).kiosk_shell;
    if (!k) continue;
    const textOk = (v: string, max: number) => {
      const n = [...v.trim()].length;
      return n > 0 && n <= max && !/[\u0000-\u001f]/.test(v);
    };
    // A packaged app (AUMID) needs no command; a desktop app needs one (2026-11-17).
    const badApp = k.apps.some((a) => {
      const target = a.app_id !== undefined ? isAppUserModelId(a.app_id) && (a.command === '' || textOk(a.command, 260)) : textOk(a.command, 260);
      return !textOk(a.title, 64) || !target;
    });
    if (k.apps.length > MAX_KIOSK_APPS || badApp || k.idle_reset_minutes > 1440 || k.idle_reset_minutes < 0) {
      out.push(issue('error', 'invalid_kiosk_shell', { seat: s.number, field: 'local.kiosk_shell' }));
    }
    if (s.number === 1 && kioskSeatOneDesktop(c)) {
      out.push(issue('warning', 'kiosk_shell_ignored_on_seat_one', { seat: s.number, field: 'local.kiosk_shell' }));
    }
  }
  // GPU preference programs (seat-core `gpu_preference_checks`).
  for (const s of c.seats) {
    const apps = localOf(s).gpu_preference?.apps ?? [];
    if (apps.length > MAX_GPU_PREF_APPS || apps.some((a) => !gpuPrefAppValid(a))) {
      out.push(issue('error', 'invalid_gpu_preference', { seat: s.number, field: 'local.gpu_preference' }));
    }
  }
  const allowClone = c.settings.outputs?.allow_clone ?? false;
  for (const d of c.devices) {
    if ((d.options?.outputs?.length ?? 0) > 1 && !allowClone) {
      out.push(issue('error', 'clone_not_allowed', { device: d.key, field: 'options.outputs' }));
    }
  }
  if (inventory) {
    const views = inventory.map((device) => ({ device, effective: 0, source: { type: 'default' } as MaskSource, arrived_ms: 0 }));
    const anyMonitor = inventory.some((d) => d.kind === 'monitor');
    for (const s of c.seats) {
      const h = seatHealth(c, s.number, views);
      if (h.readiness === 'no_display') {
        if (s.number === 1) {
          if (anyMonitor) out.push(issue('error', 'seat_one_no_display', { seat: 1 }));
        } else out.push(issue('warning', 'seat_without_display', { seat: s.number }));
      }
      if (s.number !== 1 && s.autostart) {
        const eff = effectiveMasks(c, inventory);
        const has = (k: DeviceKind) => inventory.some((d) => d.present && d.kind === k && M.contains(eff.get(d.key)!.mask, s.number));
        if (!has('keyboard')) out.push(issue('warning', 'seat_without_keyboard', { seat: s.number }));
        if (!has('mouse')) out.push(issue('warning', 'seat_without_mouse', { seat: s.number }));
      }
    }
  }
  return out;
}

export function hasErrors(issues: Issue[]): boolean {
  return issues.some((i) => i.severity === 'error');
}

export function conflicts(issues: Issue[]): Issue[] {
  return issues.filter((i) => i.severity === 'conflict');
}

/** Structural equality of two configs ignoring key order (dirty check). */
export function sameConfig(a: Config | null, b: Config | null): boolean {
  if (!a || !b) return a === b;
  return JSON.stringify(normalize(a)) === JSON.stringify(normalize(b));
}

/** Seat 1 should use the first monitor of the first adapter (spec 10 §2.3, advice only). */
export function seatOneFirstMonitorAdvice(c: Config, views: DeviceView[]): DeviceKey | null {
  const monitors = views
    .map((v) => v.device)
    .filter((d) => d.kind === 'monitor' && d.present && d.details.type === 'display');
  if (monitors.length < 2) return null;
  const first = [...monitors].sort((a, b) => (a.label ?? a.key).localeCompare(b.label ?? b.key, 'tr', { numeric: true }))[0];
  const eff = effectiveMasks(c, views.map((v) => v.device));
  return M.contains(eff.get(first.key)!.mask, 1) ? null : first.key;
}

/** Newly found monitors (from `compare_saved`) go to the spare pool (spec 10 §3.8, 2.70.6). */
export function addedMonitorsToSpare(c: Config, added: DeviceKey[], views: DeviceView[]): Config {
  let out = c;
  for (const key of added) {
    const v = views.find((x) => x.device.key === key);
    if (v && v.device.kind === 'monitor') out = setDeviceMask(out, v.device, 0);
  }
  return out;
}

/** Devices whose mask names a seat removed or above the limit go to the spare pool. */
export function dropMasksBeyondSeats(c: Config): { config: Config; moved: DeviceKey[] } {
  const out = clone(c);
  const valid = M.fromSeats(seatNumbers(out));
  const moved: DeviceKey[] = [];
  for (const d of out.devices) {
    if (M.isShared(d.mask)) continue;
    const m = M.u32(d.mask & valid);
    if (m !== M.u32(d.mask)) {
      d.mask = m;
      moved.push(d.key);
    }
  }
  return { config: out, moved };
}
