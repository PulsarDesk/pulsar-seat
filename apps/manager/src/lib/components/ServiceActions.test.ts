// SPDX-License-Identifier: GPL-3.0-only
import { render, screen, within } from '@testing-library/svelte';
import userEvent from '@testing-library/user-event';
import { describe, expect, it } from 'vitest';
import ServiceActions from './ServiceActions.svelte';
import { createDemoApi } from '#lib/demo.ts';
import { t } from '#lib/i18n.ts';
import { ManagerStore } from '#lib/store.svelte.ts';

async function setup(admin = true) {
  localStorage.clear();
  const api = createDemoApi({ admin });
  const store = new ManagerStore(api);
  await store.refreshService();
  render(ServiceActions, { store });
  return { api, store, user: userEvent.setup() };
}

describe('service start / stop / restart', () => {
  it('offers restart and stop while running', async () => {
    await setup();
    expect(screen.getByRole('button', { name: t.svc.restart })).not.toHaveAttribute('aria-disabled');
    expect(screen.getByRole('button', { name: t.svc.stop })).not.toHaveAttribute('aria-disabled');
    expect(screen.queryByRole('button', { name: t.svc.start })).toBeNull();
  });

  it('stop asks first, then stops; start brings it back', async () => {
    const { api, store, user } = await setup();
    await user.click(screen.getByRole('button', { name: t.svc.stop }));
    const dlg = screen.getByRole('alertdialog');
    expect(within(dlg).getByText(t.svc.confirmStop)).toBeInTheDocument();
    await user.click(within(dlg).getByRole('button', { name: t.svc.stop }));
    await screen.findByRole('button', { name: t.svc.start });
    expect(api.calls).toContain('service_control:stop');
    expect(store.service?.state).toBe('stopped');
    expect(store.serviceDown).toBe(true);
    expect(store.toasts.some((x) => x.text === t.svc.stopped)).toBe(true);
    await user.click(screen.getByRole('button', { name: t.svc.start }));
    await screen.findByRole('button', { name: t.svc.stop });
    expect(api.calls).toContain('service_control:start');
    expect(store.service?.state).toBe('running');
  });

  it('cancelling the restart question sends nothing', async () => {
    const { api, user } = await setup();
    await user.click(screen.getByRole('button', { name: t.svc.restart }));
    await user.click(within(screen.getByRole('alertdialog')).getByRole('button', { name: t.cancel }));
    expect(api.calls.some((c) => c.startsWith('service_control'))).toBe(false);
  });

  it('restart confirms and restarts', async () => {
    const { api, user } = await setup();
    await user.click(screen.getByRole('button', { name: t.svc.restart }));
    await user.click(within(screen.getByRole('alertdialog')).getByRole('button', { name: t.svc.restart }));
    expect(api.calls).toContain('service_control:restart');
  });

  it('both buttons are neutral; red is only on the stop confirm', async () => {
    const { user } = await setup();
    expect(screen.getByRole('button', { name: t.svc.stop })).not.toHaveClass('danger');
    expect(screen.getByRole('button', { name: t.svc.restart })).not.toHaveClass('danger');
    await user.click(screen.getByRole('button', { name: t.svc.stop }));
    const dlg = screen.getByRole('alertdialog');
    expect(within(dlg).getByRole('button', { name: t.svc.stop })).toHaveClass('danger');
    // Destructive confirms open with focus on Cancel.
    expect(within(dlg).getByRole('button', { name: t.cancel })).toHaveFocus();
  });

  it('can hand the action to the caller instead of asking', async () => {
    localStorage.clear();
    const api = createDemoApi();
    const store = new ManagerStore(api);
    await store.refreshService();
    const got: string[] = [];
    render(ServiceActions, { store, onAction: (a: string) => got.push(a) });
    await userEvent.setup().click(screen.getByRole('button', { name: t.svc.stop }));
    expect(got).toEqual(['stop']);
    expect(screen.queryByRole('alertdialog')).toBeNull();
  });

  it('an unelevated manager sees the buttons disabled with the reason', async () => {
    const { api, user } = await setup(false);
    const stop = screen.getByRole('button', { name: t.svc.stop });
    expect(stop).toHaveAttribute('aria-disabled', 'true');
    expect(stop).toHaveAccessibleDescription(t.svc.needElevated);
    expect(screen.getByText(t.svc.needElevated)).toBeVisible();
    expect(screen.getByRole('button', { name: t.svc.restart })).toHaveAttribute('aria-disabled', 'true');
    await user.click(stop);
    expect(screen.queryByRole('alertdialog')).toBeNull();
    expect(api.calls.some((c) => c.startsWith('service_control'))).toBe(false);
  });
});
