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
    await user.selectOptions(screen.getByRole('combobox', { name: t.advanced.topology }), 'dedicated_host');
    expect(store.draft?.settings.backends.host_topology).toBe('dedicated_host');
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
    await user.click(screen.getByRole('switch', { name: t.advancedPage.loopbackEnabled }));
    expect(store.draft?.settings.backends.loopback_listener?.enabled).toBe(false);
    expect(screen.queryByTestId('loopback-port')).toBeNull();
  });

  it('advanced tab: compatibility options are opt-in and edit the draft', async () => {
    const { store, user } = await setup();
    await user.click(screen.getByRole('tab', { name: t.tabs.advanced }));
    const box = screen.getByTestId('compat-settings');
    expect(box).toBeInTheDocument();
    expect(store.draft?.settings.tweaks.fast_startup_off ?? false).toBe(false);
    await user.click(screen.getByRole('switch', { name: t.compat.fastStartup }));
    expect(store.draft?.settings.tweaks.fast_startup_off).toBe(true);
    await user.click(screen.getByRole('switch', { name: t.compat.wuEnabled }));
    expect(store.draft?.settings.tweaks.windows_update?.enabled).toBe(true);
    await user.click(screen.getByRole('switch', { name: t.compat.defender }));
    expect(store.draft?.settings.tweaks.defender_exclusions).toBe(true);
    await user.click(screen.getByRole('switch', { name: t.compat.steamPerSeat }));
    expect(store.draft?.settings.apps?.steam_per_seat).toBe(true);
    await user.click(within(box).getByLabelText(t.compat.installClasses.wpd));
    expect(store.draft?.settings.tweaks.device_install_deny?.classes).toEqual(['wpd']);
    await user.click(screen.getByRole('switch', { name: t.advancedPage.classDefaults }));
    expect(store.draft?.settings.devices.class_default_descriptors).toBe(true);
    await user.click(screen.getByTestId('object-rule-add'));
    expect(store.draft?.settings.apps?.object_rules?.length).toBe(1);
    expect(screen.getByText(t.compat.objectInvalid)).toBeInTheDocument();
    await user.click(screen.getByTestId('steam-proxy-check'));
    expect(await screen.findByRole('button', { name: t.compat.steamProxyQuarantine })).toBeInTheDocument();
  });

  it('settings page: start mode and seat autostart edit the draft', async () => {
    const { store, user } = await setup();
    await user.click(screen.getByRole('tab', { name: t.tabs.basic }));
    await user.click(screen.getByRole('radio', { name: new RegExp(`^${t.settingsPage.startModeLabels.at_boot}`) }));
    expect(store.draft?.settings.start_mode).toBe('at_boot');
    expect(screen.getByText(t.startModeHints.at_boot)).toBeInTheDocument();
    // Seat 1 always starts: a plain value, no switch.
    expect(screen.getByText(t.settingsPage.alwaysOn)).toBeInTheDocument();
    expect(screen.queryByRole('switch', { name: t.settingsPage.autoStartFor(t.seatN(1)) })).toBeNull();
    await user.click(screen.getByRole('switch', { name: t.settingsPage.autoStartFor('Misafir') }));
    expect(store.draft?.seats[1].autostart).toBe(false);
  });

  it('settings page: the seat buttons have distinct names and the seat menu holds the rest', async () => {
    const { store, user } = await setup();
    await user.click(screen.getByRole('tab', { name: t.tabs.basic }));
    expect(screen.getByRole('button', { name: t.settingsPage.accountFor(t.seatN(1)) })).toBeInTheDocument();
    expect(screen.getByRole('button', { name: t.settingsPage.accountFor('Misafir') })).toBeInTheDocument();
    await user.click(screen.getByRole('button', { name: t.settingsPage.seatMore('Misafir') }));
    expect(screen.getByRole('menuitem', { name: t.settingsPage.gpuMenu })).toBeInTheDocument();
    await user.click(screen.getByRole('menuitem', { name: t.settingsPage.padsOn }));
    expect(store.draft?.seats[1].local?.virtual_pads).toBe(true);
    await user.click(screen.getByRole('button', { name: t.settingsPage.seatMore('Misafir') }));
    await user.click(screen.getByRole('menuitem', { name: t.settingsPage.padIgnoreMenu }));
    const dlg = screen.getByRole('dialog', { name: t.settingsPage.padIgnoreTitle('Misafir') });
    await user.type(within(dlg).getByTestId('pad-ignore'), '0x045E/0x0B09 nope');
    expect(within(dlg).getByText(t.settingsPage.padIgnoreBad('nope'))).toBeInTheDocument();
    await user.click(within(dlg).getByRole('button', { name: t.save }));
    expect(store.draft?.seats[1].pad_ignore_devices).toEqual(['0x045E/0x0B09']);
  });

  it('settings page links to the advanced page; its contents list marks the current section', async () => {
    const { store, user } = await setup();
    await user.click(screen.getByRole('tab', { name: t.tabs.basic }));
    await user.click(screen.getByRole('button', { name: new RegExp(`^${t.settingsPage.advanced}`) }));
    expect(store.prefs.tab).toBe('advanced');
    const toc = screen.getByRole('navigation', { name: t.advancedPage.toc });
    expect(within(toc).getByRole('link', { name: t.advancedPage.methods })).toHaveAttribute('aria-current', 'location');
    await user.click(within(toc).getByRole('link', { name: t.advancedPage.network }));
    expect(within(toc).getByRole('link', { name: t.advancedPage.network })).toHaveAttribute('aria-current', 'location');
    expect(screen.getByRole('button', { name: t.advancedPage.ipEditFor('Misafir') })).toBeInTheDocument();
    await user.click(screen.getByRole('button', { name: t.settingsPage.title }));
    expect(store.prefs.tab).toBe('basic');
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
