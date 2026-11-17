// SPDX-License-Identifier: GPL-3.0-only
import { describe, expect, it } from 'vitest';
import { applyRecord, newRemoteSeat, toRecord } from './model.ts';
import {
  defaultGuestPath,
  defaultVmSeat,
  defaultVmSettings,
  mountPointOk,
  newForward,
  newSharedFolder,
  setSeatVm,
  setVmSettings,
  vmOf,
  vmSeatProblem,
  vmSettingsOf,
  vmSettingsProblem
} from './vm.ts';
import type { Config } from '../types.ts';

const seat2 = (c: Config) => c.seats.find((s) => s.number === 2)!;

function config(): Config {
  return {
    schema_version: 3,
    revision: 1,
    seats: [newRemoteSeat(2, 'Salon')],
    devices: [],
    settings: { remote: undefined } as unknown as Config['settings']
  } as Config;
}

describe('VM seats (hyperv-seats)', () => {
  it('follows the spec mount point rule', () => {
    for (const ok of ['D:', 'D:\\', 'C:\\HostDrive', 'C:\\Games\\Steam']) expect(mountPointOk(ok), ok).toBe(true);
    for (const bad of ['C:', 'C:\\', 'C:\\Games\\', 'D:\\Games', '', 'C:\\a\\\\b', "C:\\x'y"]) expect(mountPointOk(bad), bad).toBe(false);
    expect(defaultGuestPath('E:\\')).toBe('C:\\HostDrive');
    expect(defaultGuestPath('D:\\Games\\Steam')).toBe('C:\\HostFolders\\Steam');
    expect(newSharedFolder('E:\\').guest_path).toBe('C:\\HostDrive');
  });

  it('finds the first seat problem', () => {
    const v = defaultVmSeat();
    expect(vmSeatProblem(v, 48100)).toBeNull();
    expect(vmSeatProblem({ ...v, memory_mb: 1000 }, 48100)).toBe('memory');
    expect(vmSeatProblem({ ...v, gpu: 'adapter' }, 48100)).toBe('gpuAdapter');
    expect(vmSeatProblem({ ...v, stop: 'save' }, 48100)).toBe('saveWithGpu');
    expect(vmSeatProblem({ ...v, stop: 'save', gpu: 'off' }, 48100)).toBeNull();
    const f = newSharedFolder('E:\\Games');
    expect(vmSeatProblem({ ...v, shared_folders: [f, { ...f, id: 'x' }] }, 48100)).toBe('mountTwice');
    expect(vmSeatProblem({ ...v, shared_folders: [{ ...f, guest_path: 'C:\\' }] }, 48100)).toBe('mountPoint');
    const p = { ...newForward(), host_port: 48105, guest_port: 80 };
    expect(vmSeatProblem({ ...v, forwarded_ports: [p] }, 48100)).toBe('portInStream');
    expect(vmSeatProblem({ ...v, forwarded_ports: [{ ...p, host_port: 9000 }, { ...p, id: 'b', host_port: 9000 }] }, 48100)).toBe('portTwice');
    expect(vmSeatProblem({ ...v, forwarded_ports: [{ ...p, host_port: 9000 }, { ...p, id: 'b', host_port: 9000, protocol: 'udp' }] }, 48100)).toBeNull();
    expect(vmSeatProblem({ ...v, forwarded_ports: [{ ...p, host_port: 0 }] }, 48100)).toBe('port');
  });

  it('checks the machine-wide settings', () => {
    const s = defaultVmSettings();
    expect(vmSettingsProblem(s)).toBeNull();
    expect(vmSettingsProblem({ ...s, memory_percent: 95 })).toBe('memoryPercent');
    expect(vmSettingsProblem({ ...s, image_kind: 'wim', image_path: 'install.wim' })).toBe('image');
    expect(vmSettingsProblem({ ...s, image_kind: 'wim', image_path: 'D:\\sources\\install.esd' })).toBeNull();
    expect(vmSettingsProblem({ ...s, image_kind: 'vhdx', image_path: 'D:\\x.wim' })).toBe('image');
    expect(vmSettingsProblem({ ...s, nat_prefix: '10.0.0.1/24' })).toBe('natPrefix');
    expect(vmSettingsProblem({ ...s, storage_gb: 8 })).toBe('storageGb');
  });

  it('edits the config and a seat page save keeps the VM part', () => {
    let c = config();
    expect(vmOf(seat2(c))).toBeNull();
    c = setSeatVm(c, 2, { ...defaultVmSeat(), memory_mb: 8192 });
    expect(vmOf(seat2(c))?.memory_mb).toBe(8192);
    const rec = toRecord(seat2(c));
    rec.display_name = 'Salon TV';
    const saved = applyRecord(seat2(c), rec);
    expect(saved.remote?.vm?.memory_mb).toBe(8192);
    expect(saved.remote?.display_name).toBe('Salon TV');
    c = setSeatVm(c, 2, null);
    expect(seat2(c).remote?.vm).toBeUndefined();
    c = setVmSettings(c, { ...defaultVmSettings(), memory_percent: 70 });
    expect(vmSettingsOf(c).memory_percent).toBe(70);
    expect(vmSettingsOf(null)).toEqual(defaultVmSettings());
  });
});
