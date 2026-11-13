// SPDX-License-Identifier: GPL-3.0-only
import { render, screen } from '@testing-library/svelte';
import userEvent from '@testing-library/user-event';
import { describe, expect, it, vi } from 'vitest';
import DxvkPage from './DxvkPage.svelte';
import { t } from '#lib/i18n.ts';
import type { DxvkStatus } from '#lib/types.ts';

const status = (release: string | null): DxvkStatus => ({
  release,
  games: [{ exe: String.raw`C:\Oyunlar\eski.exe`, deployed: true, version: '2.7.1', dlls: ['d3d9'], error: null }]
});

describe('DxvkPage', () => {
  it('lists games and turns DXVK off', async () => {
    const onSet = vi.fn(async () => {});
    render(DxvkPage, { load: vi.fn(async () => status('2.7.1')), onSet });
    expect(await screen.findByText(t.dxvk.release('2.7.1'))).toBeInTheDocument();
    expect(screen.getByText(t.dxvk.deployed('2.7.1', 'd3d9'))).toBeInTheDocument();
    await userEvent.click(screen.getByRole('button', { name: t.dxvk.disable }));
    expect(onSet).toHaveBeenCalledWith(String.raw`C:\Oyunlar\eski.exe`, false, []);
  });

  it('enables a picked game with the chosen DLLs and validates input', async () => {
    const onSet = vi.fn(async () => {});
    const onPickExe = vi.fn(async () => String.raw`D:\Games\yeni.exe`);
    render(DxvkPage, { load: vi.fn(async () => status('2.7.1')), onSet, onPickExe });
    await screen.findByText(t.dxvk.release('2.7.1'));
    await userEvent.click(screen.getAllByRole('button', { name: t.dxvk.enable }).at(-1)!);
    expect(await screen.findByText(t.dxvk.needExe)).toBeInTheDocument();
    await userEvent.click(screen.getByRole('button', { name: t.dxvk.browse }));
    await userEvent.click(screen.getByLabelText('Direct3D 8'));
    await userEvent.click(screen.getAllByRole('button', { name: t.dxvk.enable }).at(-1)!);
    expect(onSet).toHaveBeenCalledWith(String.raw`D:\Games\yeni.exe`, true, ['d3d9', 'd3d8']);
  });

  it('says how to download DXVK when no release is pinned', async () => {
    render(DxvkPage, { load: vi.fn(async () => status(null)), onSet: vi.fn() });
    expect(await screen.findByText(t.dxvk.noRelease)).toBeInTheDocument();
  });
});
