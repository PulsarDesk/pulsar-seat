// SPDX-License-Identifier: GPL-3.0-only
import { render, screen } from '@testing-library/svelte';
import userEvent from '@testing-library/user-event';
import { describe, expect, it, vi } from 'vitest';
import ExperimentalDialog from './ExperimentalDialog.svelte';
import { t } from '#lib/i18n.ts';
import type { TermwrapStatus } from '#lib/types.ts';

const status: TermwrapStatus = {
  state: { state: 'off' },
  termsrv_version: null,
  supported: true,
  candidates: [{ service_dll: 'C:\Program Files\RDP Wrapper\rdpwrap.dll', config_file: null, dll_version: null, signed: 'unsigned' }],
  warning_version: 1
};

describe('ExperimentalDialog', () => {
  it('shows the warning and enables only after the explicit acknowledgement', async () => {
    const onEnable = vi.fn(async () => {});
    render(ExperimentalDialog, { status, onEnable, onCancel: vi.fn() });
    expect(screen.getByTestId('warning-text')).toHaveTextContent('Deneysel özellik');
    expect(screen.getByTestId('warning-text')).toHaveTextContent('gri bir alandadır');
    const enable = screen.getByRole('button', { name: t.experimental.enable });
    expect(enable).toBeDisabled();
    const user = userEvent.setup();
    await user.click(screen.getByRole('checkbox', { name: t.experimental.ack }));
    expect(enable).toBeEnabled();
    await user.click(enable);
    expect(onEnable).toHaveBeenCalledWith('C:\Program Files\RDP Wrapper\rdpwrap.dll', 1);
  });

  it('cannot enable without a wrapper', () => {
    render(ExperimentalDialog, { status: { ...status, candidates: [] }, onEnable: vi.fn(), onCancel: vi.fn() });
    expect(screen.getByText(t.host.twNoCandidate)).toBeInTheDocument();
    expect(screen.getByRole('button', { name: t.experimental.enable })).toBeDisabled();
  });
});
