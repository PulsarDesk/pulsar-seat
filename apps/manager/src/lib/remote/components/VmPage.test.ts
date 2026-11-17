// SPDX-License-Identifier: GPL-3.0-only
import { render, screen, within } from '@testing-library/svelte';
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
      onSaveSettings: vi.fn(async () => {})
    });
    await userEvent.click(await screen.findByRole('button', { name: t.vm.prepareNetwork }));
    const dlg = screen.getByRole('alertdialog');
    expect(within(dlg).getByText(t.vm.confirmNetwork)).toBeInTheDocument();
    expect(onAction).not.toHaveBeenCalled();
    await userEvent.click(within(dlg).getByRole('button', { name: t.vm.prepareNetwork }));
    expect(onAction).toHaveBeenCalledWith({ type: 'prepare_host', confirm: true });
    expect(await screen.findByText(t.vm.networkReady)).toBeInTheDocument();
    expect(screen.getByText(t.vm.noRemoteSeats)).toBeInTheDocument();
    // No image chosen: the base disk cannot be built yet.
    expect(screen.getByRole('button', { name: t.vm.buildBase })).toHaveAttribute('aria-disabled', 'true');
  });

  it('stages the shared settings, blocks an invalid value and saves through the save bar', async () => {
    const onSaveSettings = vi.fn(async () => {});
    const user = userEvent.setup();
    render(VmPage, {
      seats: [{ number: 2, name: 'Salon', port: 48100, vm: defaultVmSeat() }],
      settings: defaultVmSettings(),
      load: vi.fn(async () => host()),
      onAction: vi.fn(async () => host()),
      onSaveSettings,
      onOpenSeat: vi.fn()
    });
    expect(screen.queryByRole('region', { name: seatT.saveBar.region })).toBeNull();
    const pct = screen.getByRole('spinbutton', { name: t.vm.memoryPercent });
    await user.clear(pct);
    await user.type(pct, '95');
    const bar = screen.getByRole('region', { name: seatT.saveBar.region });
    expect(within(bar).getByText(t.vm.problems.memoryPercent)).toBeInTheDocument();
    await user.click(within(bar).getByRole('button', { name: seatT.saveBar.save }));
    expect(onSaveSettings).not.toHaveBeenCalled();
    await user.clear(pct);
    await user.type(pct, '60');
    await user.click(within(bar).getByRole('button', { name: seatT.saveBar.save }));
    expect(onSaveSettings).toHaveBeenCalledTimes(1);
    expect((onSaveSettings.mock.calls[0] as unknown as [{ memory_percent: number }])[0].memory_percent).toBe(60);
    // The seat list links to the seat page.
    expect(screen.getByRole('button', { name: /Salon/ })).toBeInTheDocument();
  });

  it('fills the path fields from the system picker', async () => {
    const user = userEvent.setup();
    const pickPath = vi.fn(async (kind: string) => (kind === 'folder' ? 'E:/VM' : 'D:/iso/install.wim'));
    render(VmPage, {
      seats: [],
      settings: { ...defaultVmSettings(), image_kind: 'wim' },
      load: vi.fn(async () => host()),
      onAction: vi.fn(async () => host()),
      onSaveSettings: vi.fn(async () => {}),
      pickPath
    });
    await user.click(await screen.findByRole('button', { name: `${t.vm.browse} ${t.vm.imagePath}` }));
    expect(pickPath).toHaveBeenCalledWith('vm_image');
    expect(screen.getByRole('textbox', { name: t.vm.imagePath })).toHaveValue('D:/iso/install.wim');
    await user.click(screen.getByRole('button', { name: `${t.vm.browse} ${t.vm.storage}` }));
    expect(screen.getByRole('textbox', { name: t.vm.storage })).toHaveValue('E:/VM');
  });
});
