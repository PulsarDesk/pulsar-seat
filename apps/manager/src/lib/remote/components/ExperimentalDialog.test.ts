// SPDX-License-Identifier: GPL-3.0-only
// The remote host page's experimental dialog: the shared `components/ExperimentalDialog` in its
// `remote` variant (redesign P8 removed the remote copy). The basic accept / no-wrapper cases
// live in `components/dialogs.test.ts`; these cover the remote-only parts.
import { render, screen } from '@testing-library/svelte';
import userEvent from '@testing-library/user-event';
import { describe, expect, it, vi } from 'vitest';
import ExperimentalDialog from '#lib/components/ExperimentalDialog.svelte';
import { rt as t } from '#lib/i18n.ts';
import type { TermwrapStatus } from '#lib/remote/types.ts';

const status: TermwrapStatus = {
  state: { state: 'off' },
  termsrv_version: null,
  supported: true,
  candidates: [
    { service_dll: 'C:\\Program Files\\RDP Wrapper\\rdpwrap.dll', config_file: null, dll_version: null, signed: 'unsigned' },
    { service_dll: 'C:\\Tools\\TermWrap\\termwrap.dll', config_file: null, dll_version: null, signed: 'unsigned' }
  ],
  warning_version: 3
};

describe('ExperimentalDialog (remote variant)', () => {
  it('shows the warning and enables the chosen wrapper with the service warning version', async () => {
    const onEnable = vi.fn(async () => {});
    render(ExperimentalDialog, { variant: 'remote', status, onEnable, onCancel: vi.fn() });
    expect(screen.getByTestId('warning-text')).toHaveTextContent('Experimental feature');
    expect(screen.getByTestId('warning-text')).toHaveTextContent('grey area');
    const user = userEvent.setup();
    await user.selectOptions(screen.getByRole('combobox', { name: t.experimental.wrapper }), 'C:\\Tools\\TermWrap\\termwrap.dll');
    await user.click(screen.getByRole('checkbox', { name: t.experimental.ack }));
    await user.click(screen.getByRole('button', { name: t.experimental.enable }));
    expect(onEnable).toHaveBeenCalledWith('C:\\Tools\\TermWrap\\termwrap.dll', 3);
  });

  it('shows a failed enable as an error and stays open', async () => {
    const onEnable = vi.fn(async () => {
      throw { code: 'access_denied', message: 'demo' };
    });
    render(ExperimentalDialog, { variant: 'remote', status, onEnable, onCancel: vi.fn() });
    const user = userEvent.setup();
    await user.click(screen.getByRole('checkbox', { name: t.experimental.ack }));
    await user.click(screen.getByRole('button', { name: t.experimental.enable }));
    expect(await screen.findByRole('alert')).toBeInTheDocument();
    expect(screen.getByRole('alertdialog')).toBeInTheDocument();
  });
});
