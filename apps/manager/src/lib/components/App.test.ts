// SPDX-License-Identifier: GPL-3.0-only
import { render, screen, within } from '@testing-library/svelte';
import userEvent from '@testing-library/user-event';
import { tick } from 'svelte';
import { describe, expect, it } from 'vitest';
import App from './App.svelte';
import { createDemoApi } from '#lib/demo.ts';
import { t } from '#lib/i18n.ts';
import { ManagerStore } from '#lib/store.svelte.ts';
import { EXPERIMENTAL_WARNING_VERSION } from '#lib/types.ts';

async function setup() {
  localStorage.clear();
  const api = createDemoApi();
  const store = new ManagerStore(api);
  await store.init();
  render(App, { store });
  const user = userEvent.setup();
  // The "configured but disabled" offer opens on load; dismiss it.
  await user.click(await screen.findByRole('button', { name: t.later }));
  return { api, store, user };
}

describe('App', () => {
  it('shows the product state, seats and the system pool', async () => {
    await setup();
    expect(screen.getByTestId('product-state')).toHaveTextContent('Off');
    expect(screen.getByRole('region', { name: t.canvas.system })).toBeInTheDocument();
    const two = screen.getByRole('region', { name: `2. ${t.seat}` });
    expect(within(two).getByText('Kablosuz klavye')).toBeInTheDocument();
    expect(within(two).getByText(t.health.ready)).toBeInTheDocument();
  });

  it('adding a seat enables Save; saving writes the config', async () => {
    const { api, user } = await setup();
    const save = screen.getByRole('button', { name: t.bar.save });
    expect(save).toBeDisabled();
    await user.click(screen.getByRole('button', { name: t.canvas.addSeat }));
    expect(save).toBeEnabled();
    expect(screen.getByRole('region', { name: `3. ${t.seat}` })).toBeInTheDocument();
    await user.click(save);
    expect(api.state.config.seats).toHaveLength(3);
    expect(save).toBeDisabled();
  });

  it('enable asks first, then start seats works', async () => {
    const { api, user } = await setup();
    await user.click(screen.getByRole('button', { name: t.bar.enable }));
    const dlg = screen.getByRole('alertdialog');
    await user.click(within(dlg).getByRole('button', { name: t.bar.enable }));
    expect(api.state.product.state).toBe('enabled');
    await user.click(screen.getByRole('button', { name: t.bar.start }));
    const confirm = screen.getByRole('alertdialog');
    await user.click(within(confirm).getByRole('button', { name: t.bar.start }));
    expect(api.calls).toContain('start_seats');
  });

  it('advanced tab: experimental backend needs the warning, input isolation note', async () => {
    const { store, user } = await setup();
    await user.click(screen.getByRole('tab', { name: t.tabs.advanced }));
    expect(screen.getByTestId('input-not-isolated')).toBeInTheDocument();
    await user.click(screen.getByLabelText(/Multiple sessions on client Windows/));
    const dlg = screen.getByRole('alertdialog');
    expect(store.draft?.settings.backends.session).toBe('rds_loopback');
    await user.click(within(dlg).getByRole('checkbox', { name: t.experimental.ack }));
    await user.click(within(dlg).getByRole('button', { name: t.experimental.enable }));
    expect(store.draft?.settings.backends.session).toBe('termsrv_wrapper');
    expect(store.draft?.settings.backends.experimental_ack).toBe(EXPERIMENTAL_WARNING_VERSION);
    await user.click(screen.getByLabelText(t.backendNames.dedicated_host));
    expect(screen.queryByTestId('input-not-isolated')).toBeNull();
  });

  it('advanced tab: passwordless-seat listener settings edit the draft', async () => {
    const { store, user } = await setup();
    await user.click(screen.getByRole('tab', { name: t.tabs.advanced }));
    expect(screen.getByTestId('loopback-listener')).toBeInTheDocument();
    const port = screen.getByTestId('loopback-port') as HTMLInputElement;
    await user.clear(port);
    await user.type(port, '3389');
    port.dispatchEvent(new Event('change', { bubbles: true }));
    expect(store.draft?.settings.backends.loopback_listener?.port).toBe(3389);
    await tick();
    expect(port.getAttribute('aria-invalid')).toBe('true');
    const accounts = screen.getByTestId('loopback-accounts') as HTMLTextAreaElement;
    await user.type(accounts, 'misafir{Enter}EV\\veli');
    accounts.dispatchEvent(new Event('change', { bubbles: true }));
    expect(store.draft?.settings.backends.loopback_listener?.extra_accounts).toEqual(['misafir', 'EV\\veli']);
    await user.click(screen.getByTestId('loopback-enabled'));
    expect(store.draft?.settings.backends.loopback_listener?.enabled).toBe(false);
    expect(screen.getByTestId('loopback-port')).toBeDisabled();
  });

  it('basic tab: start mode and seat autostart edit the draft', async () => {
    const { store, user } = await setup();
    await user.click(screen.getByRole('tab', { name: t.tabs.basic }));
    await user.click(screen.getByLabelText(t.startModes.at_boot));
    expect(store.draft?.settings.start_mode).toBe('at_boot');
    expect(screen.getByText(t.startModeHints.at_boot)).toBeInTheDocument();
    expect(screen.getByLabelText(`1. ${t.basic.colStart}`)).toBeDisabled();
    await user.click(screen.getByLabelText(`2. ${t.basic.colStart}`));
    expect(store.draft?.seats[1].autostart).toBe(false);
  });

  it('device menu sends a device to the spare pool', async () => {
    const { store, user } = await setup();
    const two = screen.getByRole('region', { name: `2. ${t.seat}` });
    await user.click(within(two).getByRole('button', { name: `${t.kinds.keyboard}: Kablosuz klavye` }));
    await user.click(screen.getByRole('menuitem', { name: t.deviceMenu.spare }));
    expect(store.draft?.devices.find((d) => d.key === 'kbd;loc=USB(2-1-1)')?.mask).toBe(0);
    const sys = screen.getByRole('region', { name: t.canvas.system });
    expect(within(sys).getByText('Kablosuz klavye')).toBeInTheDocument();
  });
});
