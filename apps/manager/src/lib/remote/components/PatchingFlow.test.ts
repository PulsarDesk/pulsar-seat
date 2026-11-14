// SPDX-License-Identifier: GPL-3.0-only
import { render, screen } from '@testing-library/svelte';
import userEvent from '@testing-library/user-event';
import { describe, expect, it, vi } from 'vitest';
import PatchingFlow from './PatchingFlow.svelte';
import { rt as t } from '#lib/i18n.ts';

describe('PatchingFlow', () => {
  it('install: confirm with warning, apply, then offer the reboot through the service', async () => {
    const onApply = vi.fn(async () => {});
    const onReboot = vi.fn(async () => {});
    const onClose = vi.fn();
    render(PatchingFlow, { enable: true, onApply, onReboot, onClose, minProgressMs: 0 });
    expect(screen.getByText(t.patching.install.title)).toBeInTheDocument();
    expect(screen.getAllByRole('listitem')).toHaveLength(3);
    expect(screen.getByTestId('patch-caution')).toHaveTextContent(t.patching.caution);
    expect(onApply).not.toHaveBeenCalled();
    const user = userEvent.setup();
    await user.click(screen.getByRole('button', { name: t.patching.install.confirm }));
    expect(onApply).toHaveBeenCalledOnce();
    await user.click(await screen.findByRole('button', { name: t.patching.rebootNow }));
    expect(onReboot).toHaveBeenCalledOnce();
    expect(onClose).toHaveBeenCalled();
  });

  it('remove: negated bullets, no caution text; cancel applies nothing', async () => {
    const onApply = vi.fn(async () => {});
    const onClose = vi.fn();
    render(PatchingFlow, { enable: false, onApply, onReboot: vi.fn(), onClose, minProgressMs: 0 });
    expect(screen.getByText(t.patching.remove.bullets[0])).toBeInTheDocument();
    expect(screen.queryByTestId('patch-caution')).toBeNull();
    await userEvent.setup().click(screen.getByRole('button', { name: t.patching.cancel }));
    expect(onApply).not.toHaveBeenCalled();
    expect(onClose).toHaveBeenCalled();
  });
});
