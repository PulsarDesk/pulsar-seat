// SPDX-License-Identifier: GPL-3.0-only
import { render, screen } from '@testing-library/svelte';
import userEvent from '@testing-library/user-event';
import { describe, expect, it, vi } from 'vitest';
import CredentialTestDialog from './CredentialTestDialog.svelte';
import { rt as t } from '#lib/i18n.ts';
import type { CredentialTest } from '#lib/remote/types.ts';

const ok: CredentialTest = { ok: true, disabled: false, locked_out: false, password_expired: false };

describe('CredentialTestDialog', () => {
  it('shows success after the background test', async () => {
    render(CredentialTestDialog, { onTest: vi.fn(async () => ok), onEnableUnlock: vi.fn(), onClose: vi.fn() });
    expect(await screen.findByText(t.cred.okTitle)).toBeInTheDocument();
    expect(screen.queryByRole('button', { name: t.cred.fix })).toBeNull();
  });

  it('shows the Windows error and unlocks only after confirmation', async () => {
    const locked: CredentialTest = {
      ok: false,
      error_code: 1909,
      message: 'Başvurulan hesap şu anda kilitli.',
      disabled: false,
      locked_out: true,
      password_expired: false
    };
    const onTest = vi.fn().mockResolvedValueOnce(locked).mockResolvedValueOnce(ok);
    const onEnableUnlock = vi.fn(async () => {});
    render(CredentialTestDialog, { onTest, onEnableUnlock, onClose: vi.fn() });
    expect(await screen.findByText(locked.message!)).toBeInTheDocument();
    expect(screen.getByTestId('cred-locked')).toBeInTheDocument();
    const user = userEvent.setup();
    await user.click(screen.getByRole('button', { name: t.cred.fix }));
    // Nothing changed yet: the confirmation screen comes first.
    expect(onEnableUnlock).not.toHaveBeenCalled();
    expect(screen.getByText(t.cred.confirmBody)).toBeInTheDocument();
    await user.click(screen.getByRole('button', { name: t.cred.confirm }));
    expect(onEnableUnlock).toHaveBeenCalledWith(false, true);
    expect(await screen.findByText(t.cred.okTitle)).toBeInTheDocument();
    expect(onTest).toHaveBeenCalledTimes(2);
  });
});
