// SPDX-License-Identifier: GPL-3.0-only
import { render, screen } from '@testing-library/svelte';
import userEvent from '@testing-library/user-event';
import { describe, expect, it, vi } from 'vitest';
import VmPage from './VmPage.svelte';
import { rt as t, t as seatT } from '#lib/i18n.ts';
import type { VmHostStatus } from '#lib/types.ts';
import { defaultVmSeat, defaultVmSettings } from '#lib/remote/vm.ts';

const host = (over: Partial<VmHostStatus> = {}): VmHostStatus => ({
  feature: 'enabled',
  vmms: true,
  network_ready: false,
  base_present: false,
  building: false,
  storage: 'C:\\ProgramData\\Pulsar Seat\\vm',
  gpus: [],
  host_memory_mb: 32768,
  logical_processors: 16,
  seats: [],
  ...over
});

describe('VmPage', () => {
  it('sets up the VM network only after the confirmation', async () => {
    const onAction = vi.fn(async () => host({ network_ready: true }));
    render(VmPage, {
      seats: [],
      settings: defaultVmSettings(),
      load: vi.fn(async () => host()),
      onAction,
      onSaveSettings: vi.fn(async () => {}),
      onSaveSeat: vi.fn(async () => {})
    });
    await userEvent.click(await screen.findByRole('button', { name: t.vm.prepareNetwork }));
    expect(screen.getByText(t.vm.confirmNetwork)).toBeInTheDocument();
    expect(onAction).not.toHaveBeenCalled();
    await userEvent.click(screen.getByRole('button', { name: seatT.ok }));
    expect(onAction).toHaveBeenCalledWith({ type: 'prepare_host', confirm: true });
    expect(await screen.findByText(new RegExp(t.vm.networkReady))).toBeInTheDocument();
    expect(screen.getByText(t.vm.noRemoteSeats)).toBeInTheDocument();
    // No image chosen: the base disk cannot be built yet.
    expect(screen.getByRole('button', { name: t.vm.buildBase })).toBeDisabled();
  });

  it('turns VM mode on for a seat, adds a folder and saves', async () => {
    const onSaveSeat = vi.fn(async () => {});
    render(VmPage, {
      seats: [{ number: 2, name: 'Salon', port: 48100, vm: null }],
      settings: defaultVmSettings(),
      load: vi.fn(async () => host({ seats: [] })),
      onAction: vi.fn(async () => host()),
      onSaveSettings: vi.fn(async () => {}),
      onSaveSeat
    });
    await userEvent.click(await screen.findByLabelText(t.vm.vmMode));
    await userEvent.type(screen.getByLabelText(t.vm.hostFolder), 'E:\\');
    await userEvent.click(screen.getByRole('button', { name: t.vm.addFolder }));
    expect(screen.getByDisplayValue('C:\\HostDrive')).toBeInTheDocument();
    await userEvent.click(screen.getByRole('button', { name: t.vm.addForward }));
    // A new forward has no ports yet: saving is blocked with the reason.
    expect(screen.getByText(t.vm.problems.port)).toBeInTheDocument();
    expect(screen.getByRole('button', { name: t.vm.saveSeat })).toBeDisabled();
    // Remove the forward again (the last Remove button), then save.
    await userEvent.click(screen.getAllByRole('button', { name: t.vm.remove }).at(-1)!);
    await userEvent.click(screen.getByRole('button', { name: t.vm.saveSeat }));
    expect(onSaveSeat).toHaveBeenCalledTimes(1);
    const [seat, vm] = onSaveSeat.mock.calls[0] as unknown as [number, ReturnType<typeof defaultVmSeat>];
    expect(seat).toBe(2);
    expect(vm.shared_folders.map((f) => [f.host_path, f.guest_path, f.read_only])).toEqual([['E:\\', 'C:\\HostDrive', true]]);
    expect(vm.forwarded_ports).toEqual([]);
  });
});
