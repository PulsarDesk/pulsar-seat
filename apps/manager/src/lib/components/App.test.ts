// SPDX-License-Identifier: GPL-3.0-only
import { render, screen, within } from '@testing-library/svelte';
import userEvent from '@testing-library/user-event';
import { tick } from 'svelte';
import { describe, expect, it } from 'vitest';
import App from './App.svelte';
import { createDemoApi } from '#lib/demo.ts';
import { t } from '#lib/i18n.ts';
import { ManagerStore } from '#lib/store.svelte.ts';
import type { TrayAction } from '#lib/api.ts';
import { EXPERIMENTAL_WARNING_VERSION } from '#lib/types.ts';

async function setup(api = createDemoApi()) {
  localStorage.clear();
  const store = new ManagerStore(api);
  await store.init();
  render(App, { store });
  const user = userEvent.setup();
  await screen.findByRole('region', { name: t.canvas.system });
  return { api, store, user };
}

const nav = (name: string) => screen.getByRole('link', { name: new RegExp(`^${name}`) });
const saveBar = () => screen.queryByRole('region', { name: t.saveBar.region });
/** Tray items reach App through the store's handler (the shell sends them). */
const tray = (store: ManagerStore, a: TrayAction) => (store as unknown as { trayHandler: (a: TrayAction) => void }).trayHandler(a);

describe('App shell', () => {
  it('renders the rail, the seats and the system pool, with no startup modal', async () => {
    await setup();
    // BEHAVIOUR (P2): the "configured but off" offer no longer opens on launch.
    expect(screen.queryByRole('alertdialog')).toBeNull();
    expect(screen.getByRole('complementary', { name: t.nav.app })).toBeInTheDocument();
    expect(screen.getByRole('main')).toBeInTheDocument();
    expect(nav(t.nav.seats)).toHaveAttribute('aria-current', 'page');
    expect(nav(t.nav.remote)).not.toHaveAttribute('aria-current');
    expect(screen.queryByRole('tab')).toBeNull();
    // Seat cards are named by the seat's title ("Misafir"; an unnamed seat is "Seat N").
    const two = screen.getByRole('region', { name: 'Misafir' });
    expect(within(two).getByText('Kablosuz klavye')).toBeInTheDocument();
    expect(within(two).getByText(t.seatN(2), { exact: false })).toBeInTheDocument();
  });

  it('the Status link names the service state', async () => {
    const { store } = await setup();
    expect(nav(t.nav.status)).toHaveAccessibleName(`${t.nav.status}${t.nav.statusOk}`);
    store.serviceDown = true;
    await tick();
    expect(nav(t.nav.status)).toHaveAccessibleName(`${t.nav.status}${t.nav.statusDown}`);
  });

  it('rail links switch views; Settings stays current on its Advanced sub-page', async () => {
    const { store, user } = await setup();
    await user.click(nav(t.nav.settings));
    expect(store.prefs.tab).toBe('settings');
    expect(nav(t.nav.settings)).toHaveAttribute('aria-current', 'page');
    store.setPref('tab', 'advanced');
    await tick();
    expect(nav(t.nav.settings)).toHaveAttribute('aria-current', 'page');
    await user.click(nav(t.nav.status));
    expect(store.prefs.tab).toBe('status');
    expect(nav(t.nav.status)).toHaveAttribute('aria-current', 'page');
  });

  it('the save bar appears with a staged change; saving writes the config', async () => {
    const { api, user } = await setup();
    expect(saveBar()).toBeNull();
    await user.click(screen.getByRole('button', { name: t.canvas.addSeat }));
    const bar = saveBar()!;
    expect(bar).toBeInTheDocument();
    expect(within(bar).getByText(t.saveBar.count(1))).toBeInTheDocument();
    expect(t.saveBar.count(1).endsWith('.')).toBe(false);
    expect(screen.getByRole('region', { name: t.seatN(3) })).toBeInTheDocument();
    // Docked: the bar is a sibling after the scroll area inside main, not inside it.
    expect(bar.parentElement).toBe(screen.getByRole('main'));
    expect(bar.previousElementSibling).toHaveClass('scroll');
    await user.click(within(bar).getByRole('button', { name: t.saveBar.save }));
    expect(api.state.config.seats).toHaveLength(3);
    expect(saveBar()).toBeNull();
    // The confirmation is a toast without buttons.
    const toast = await screen.findByText(t.saved.ok);
    expect(within(toast.closest('.toast') as HTMLElement).queryByRole('button')).toBeNull();
  });

  it('undo in the save bar drops the staged change', async () => {
    const { store, user } = await setup();
    await user.click(screen.getByRole('button', { name: t.canvas.addSeat }));
    await user.click(within(saveBar()!).getByRole('button', { name: t.saveBar.undo }));
    expect(store.dirty).toBe(false);
    expect(saveBar()).toBeNull();
  });

  it('turn on asks first, then starts the seats', async () => {
    const { api, store, user } = await setup();
    tray(store, 'toggle_enabled');
    await tick();
    const dlg = screen.getByRole('alertdialog');
    await user.click(within(dlg).getByRole('button', { name: t.power.openAndStart }));
    expect(api.state.product.state).toBe('enabled');
    // Turning on starts the seats (the former startup offer): the start question follows.
    const confirm = await screen.findByRole('alertdialog', { name: t.start.confirmTitle });
    await user.click(within(confirm).getByRole('button', { name: t.bar.start }));
    expect(api.calls).toContain('start_seats');
  });

  it("the status line's Start seats goes through the start question", async () => {
    const { api, store, user } = await setup();
    tray(store, 'toggle_enabled');
    await tick();
    await user.click(within(screen.getByRole('alertdialog')).getByRole('button', { name: t.power.openAndStart }));
    // Dismiss the start question that follows turning on; the seats stay stopped.
    await screen.findByRole('alertdialog', { name: t.start.confirmTitle });
    await user.keyboard('{Escape}');
    await tick();
    expect(screen.queryByRole('alertdialog')).toBeNull();
    expect(api.calls).not.toContain('start_seats');
    await user.click(screen.getAllByRole('button', { name: t.bar.start })[0]);
    const confirm = await screen.findByRole('alertdialog', { name: t.start.confirmTitle });
    await user.click(within(confirm).getByRole('button', { name: t.bar.start }));
    expect(api.calls).toContain('start_seats');
  });

  it('service down replaces the page with the recovery state', async () => {
    const { store } = await setup();
    store.serviceDown = true;
    await tick();
    const alert = screen.getByRole('alert');
    expect(within(alert).getByRole('heading', { name: t.service.downTitle })).toBeInTheDocument();
    expect(within(alert).getByRole('button', { name: t.service.start })).toBeInTheDocument();
    expect(within(alert).getByRole('button', { name: t.service.diagnostics })).toBeInTheDocument();
    expect(screen.queryByRole('region', { name: t.canvas.system })).toBeNull();
  });

  it('page notices share one region; failures stay until OK', async () => {
    const { store, user } = await setup();
    store.toast('error', 'Broken', 'x_code');
    await tick();
    const one = screen.getByRole('alert');
    expect(one).toHaveTextContent('Broken (x_code)');
    store.configChangedElsewhere = true;
    await tick();
    expect(screen.getByText(t.notices.count(2))).toBeInTheDocument();
    const okButtons = screen.getAllByRole('button', { name: t.notices.ok });
    await user.click(okButtons[0]);
    expect(store.toasts.some((x) => x.kind === 'error')).toBe(false);
    expect(screen.queryByText(t.notices.count(2))).toBeNull();
    expect(screen.getByText(t.changedElsewhere)).toBeInTheDocument();
  });

  it('a seat that must restart after saving is kept in the store, not toasted', async () => {
    const api = createDemoApi();
    const apply = api.apply.bind(api);
    api.apply = async (...a: Parameters<typeof api.apply>) => ({ ...(await apply(...a)), restart_seats: [2] });
    const { store, user } = await setup(api);
    await user.click(screen.getByRole('button', { name: t.canvas.addSeat }));
    await user.click(within(saveBar()!).getByRole('button', { name: t.saveBar.save }));
    expect(store.restartRequired).toEqual([2]);
    expect(store.toasts.some((x) => x.text.includes(t.saved.restartSeats('2')))).toBe(false);
    api.state.product = { state: 'enabled', started: true };
    await store.restartSeat(2);
    expect(api.calls).toEqual(expect.arrayContaining(['stop_seat', 'start_seats']));
    expect(store.restartRequired).toEqual([]);
  });

  it('advanced view: experimental backend needs the warning, input isolation note', async () => {
    const { store, user } = await setup();
    store.setPref('tab', 'advanced');
    await tick();
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

  it('advanced view: passwordless-seat listener settings edit the draft', async () => {
    const { store, user } = await setup();
    store.setPref('tab', 'advanced');
    await tick();
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

  it('advanced view: compatibility options are opt-in and edit the draft', async () => {
    const { store, user } = await setup();
    store.setPref('tab', 'advanced');
    await tick();
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
    await user.click(nav(t.nav.settings));
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
    await user.click(nav(t.nav.settings));
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

  it('settings seat menu IP dialog opens the program list on the advanced page', async () => {
    const { store, user } = await setup();
    await user.click(nav(t.nav.settings));
    await user.click(screen.getByRole('button', { name: t.settingsPage.seatMore('Misafir') }));
    await user.click(screen.getByRole('menuitem', { name: new RegExp(`^${t.settingsPage.ipMenu}`) }));
    const dlg = await screen.findByRole('dialog', { name: t.ipDialog.title(2) });
    await user.click(within(dlg).getByRole('button', { name: t.ipDialog.openList }));
    expect(store.prefs.tab).toBe('advanced');
    expect(screen.queryByRole('dialog', { name: t.ipDialog.title(2) })).not.toBeInTheDocument();
    expect(screen.getByTestId('ip-program-add')).toBeInTheDocument();
  });

  it('settings page links to the advanced page; its contents list marks the current section', async () => {
    const { store, user } = await setup();
    await user.click(nav(t.nav.settings));
    await user.click(screen.getByRole('button', { name: new RegExp(`^${t.settingsPage.advanced}`) }));
    expect(store.prefs.tab).toBe('advanced');
    const toc = screen.getByRole('navigation', { name: t.advancedPage.toc });
    expect(within(toc).getByRole('link', { name: t.advancedPage.methods })).toHaveAttribute('aria-current', 'location');
    await user.click(within(toc).getByRole('link', { name: t.advancedPage.network }));
    expect(within(toc).getByRole('link', { name: t.advancedPage.network })).toHaveAttribute('aria-current', 'location');
    expect(screen.getByRole('button', { name: t.advancedPage.ipEditFor('Misafir') })).toBeInTheDocument();
    await user.click(screen.getByRole('button', { name: t.settingsPage.title }));
    expect(store.prefs.tab).toBe('settings');
  });

  it('device menu sends a device to the spare pool', async () => {
    const { store, user } = await setup();
    const two = screen.getByRole('region', { name: 'Misafir' });
    await user.click(within(two).getByRole('button', { name: t.device.actions('Kablosuz klavye') }));
    await user.click(screen.getByRole('menuitem', { name: t.move.toPool }));
    expect(store.draft?.devices.find((d) => d.key === 'kbd;loc=USB(2-1-1)')?.mask).toBe(0);
    const sys = screen.getByRole('region', { name: t.canvas.system });
    expect(within(sys).getByText('Kablosuz klavye')).toBeInTheDocument();
  });
});
