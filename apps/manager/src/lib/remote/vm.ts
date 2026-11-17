// SPDX-License-Identifier: GPL-3.0-only
// Hyper-V VM seats (hyperv-seats 2026-11-17; spec/remote 10 §8): the manager's mirror of
// `seat_core::remote::vm` (defaults, mount point rule, port checks) and the config edits of
// the VM seats page. The service validates again on `apply_config`.

import type { Config, SeatConfig, SeatNumber } from '../types.ts';
import { clone, normalize } from '../model.ts';
import type { ForwardedPort, SharedFolder, VmSeat, VmSettings } from './types.ts';

export const DEFAULT_NAT_PREFIX = '172.31.240.0/24';
export const HOST_DRIVE_MOUNT = 'C:\\HostDrive';
export const MIN_MEMORY_MB = 2048;

/** `seat_core::remote::vm::VmSettings::default()`. */
export function defaultVmSettings(): VmSettings {
  return {
    storage_location: '',
    memory_percent: 50,
    storage_gb: 128,
    image_kind: 'none',
    image_path: '',
    image_index: 1,
    language_cab: '',
    restart_gpus_on_shutdown: false,
    nat_prefix: DEFAULT_NAT_PREFIX
  };
}

/** `VmSeat::default()`. */
export function defaultVmSeat(): VmSeat {
  return {
    memory_mb: 0,
    processors: 0,
    gpu: 'auto',
    shared_folders: [],
    forwarded_ports: [],
    hide_vm_suffix: false,
    stop: 'shut_down',
    guest_address: ''
  };
}

export function vmSettingsOf(c: Config | null | undefined): VmSettings {
  return { ...defaultVmSettings(), ...(c?.settings.remote?.vm ?? {}) };
}

export function vmOf(s: SeatConfig | undefined): VmSeat | null {
  const v = s?.remote?.vm;
  return v ? { ...defaultVmSeat(), ...v, shared_folders: v.shared_folders ?? [], forwarded_ports: v.forwarded_ports ?? [] } : null;
}

export function isVmSeat(s: SeatConfig | undefined): boolean {
  return !!s?.remote?.vm;
}

/** The config with seat `n`'s VM part set (`null` = back to an RDP loopback seat). */
export function setSeatVm(c: Config, n: SeatNumber, vm: VmSeat | null): Config {
  const out = clone(c);
  const s = out.seats.find((x) => x.number === n);
  if (s?.remote) {
    if (vm) s.remote.vm = clone(vm);
    else delete s.remote.vm;
  }
  return normalize(out);
}

export function setVmSettings(c: Config, v: VmSettings): Config {
  const out = clone(c);
  out.settings.remote = { ...(out.settings.remote ?? ({} as NonNullable<typeof out.settings.remote>)), vm: { ...v } };
  return out;
}

const UNSAFE = /[\u0000-\u001f"'`$]/;

/** Absolute local Windows path (`X:\...`), no quotes or `..`. */
export function absPathOk(p: string): boolean {
  return /^[A-Za-z]:\\/.test(p) && p.length <= 260 && !UNSAFE.test(p) && !p.split('\\').includes('..');
}

/** Spec 10 §6 mount point rule: a drive letter other than C:, or a `C:\` path with at least
 *  one folder and no trailing backslash. */
export function mountPointOk(p: string): boolean {
  if (/^[A-Za-z]:\\?$/.test(p)) return p[0].toUpperCase() !== 'C';
  return /^[Cc]:\\[^\\]/.test(p) && !p.endsWith('\\') && !p.includes('\\\\') && absPathOk(p) && !/[:*?<>|/]/.test(p.slice(3));
}

/** `C:\HostDrive` for a drive root, else `C:\HostFolders\<last folder>`. */
export function defaultGuestPath(host: string): string {
  const t = host.replace(/\\+$/, '');
  if (/^[A-Za-z]:$/.test(t)) return HOST_DRIVE_MOUNT;
  return `C:\\HostFolders\\${t.split('\\').pop() || 'Shared'}`;
}

function newId(): string {
  const c = globalThis.crypto;
  if (c?.randomUUID) return c.randomUUID();
  return `${Date.now().toString(16)}-${Math.random().toString(16).slice(2)}`;
}

export function newSharedFolder(host: string): SharedFolder {
  return { id: newId(), host_path: host, guest_path: defaultGuestPath(host), read_only: true };
}

export function newForward(): ForwardedPort {
  return { id: newId(), description: '', protocol: 'tcp', host_port: 0, guest_port: 0 };
}

/** The seat's stream block `port-5 ..= port+21`. */
export function inStreamBlock(port: number, base: number): boolean {
  return base > 0 && port >= base - 5 && port <= base + 21;
}

/** First problem of a VM seat (a `vm` locale key), or `null`. */
export function vmSeatProblem(v: VmSeat, base: number): string | null {
  if (v.memory_mb !== 0 && v.memory_mb < MIN_MEMORY_MB) return 'memory';
  if (v.gpu === 'adapter' && !v.gpu_adapter) return 'gpuAdapter';
  if (v.stop === 'save' && v.gpu !== 'off') return 'saveWithGpu';
  const mounts = new Set<string>();
  for (const f of v.shared_folders) {
    if (!absPathOk(f.host_path)) return 'hostPath';
    if (!mountPointOk(f.guest_path)) return 'mountPoint';
    const k = f.guest_path.toLowerCase();
    if (mounts.has(k)) return 'mountTwice';
    mounts.add(k);
  }
  const hostPorts = new Set<string>();
  for (const p of v.forwarded_ports) {
    if (!(p.host_port >= 1 && p.host_port <= 65535 && p.guest_port >= 1 && p.guest_port <= 65535)) return 'port';
    if (inStreamBlock(p.host_port, base)) return 'portInStream';
    const k = `${p.protocol}:${p.host_port}`;
    if (hostPorts.has(k)) return 'portTwice';
    hostPorts.add(k);
  }
  return null;
}

/** First problem of the global settings (a `vm` locale key), or `null`. */
export function vmSettingsProblem(s: VmSettings): string | null {
  if (s.storage_location && !absPathOk(s.storage_location)) return 'storage';
  if (s.memory_percent < 10 || s.memory_percent > 90) return 'memoryPercent';
  if (s.storage_gb < 32 || s.storage_gb > 4096) return 'storageGb';
  if (s.image_kind === 'wim' && !(absPathOk(s.image_path) && /\.(wim|esd)$/i.test(s.image_path))) return 'image';
  if (s.image_kind === 'vhdx' && !(absPathOk(s.image_path) && /\.vhdx$/i.test(s.image_path))) return 'image';
  if (s.language_cab && !(absPathOk(s.language_cab) && /\.cab$/i.test(s.language_cab))) return 'languageCab';
  if (!/^\d{1,3}\.\d{1,3}\.\d{1,3}\.0\/(1[6-9]|2[0-8])$/.test(s.nat_prefix)) return 'natPrefix';
  return null;
}
