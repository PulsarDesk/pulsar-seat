// SPDX-License-Identifier: GPL-3.0-only
// Tab-leave guard, network addresses + IP dialog, outputs, pictures, wizard pages.
import { render, screen, within } from '@testing-library/svelte';
import userEvent from '@testing-library/user-event';
import { describe, expect, it, vi } from 'vitest';
import App from './App.svelte';
import NetworkSection from './NetworkSection.svelte';
import OutputsDialog from './OutputsDialog.svelte';
import PictureDialog from './PictureDialog.svelte';
import Wizard from './Wizard.svelte';
import WizardPage from './WizardPage.svelte';
import { createDemoApi, demoConfig, demoInventory } from '#lib/demo.ts';
import { t } from '#lib/i18n.ts';
import { normalize } from '#lib/model.ts';
import { ManagerStore } from '#lib/store.svelte.ts';

async function setup() {
  localStorage.clear();
  const api = createDemoApi();
  const store = new ManagerStore(api);
  await store.init();
  render(App, { store });
  const user = userEvent.setup();
  await screen.findByRole('region', { name: t.canvas.system });
  return { api, store, user };
}

/** The network addresses section of the Advanced page, on its own. */
async function setupNetwork() {
  localStorage.clear();
  const api = createDemoApi();
  const store = new ManagerStore(api);
  await store.init();
  const user = userEvent.setup();
  const view = render(NetworkSection, { store, readOnly: !store.isAdmin });
  return { api, store, user, view };
}

const navLink = (name: string) => screen.getByRole('link', { name: new RegExp(`^${name}`) });

describe('tab leave with unsaved changes', () => {
  it('asks save / discard / cancel and follows the answer', async () => {
    const { store, user } = await setup();
    await user.click(screen.getByRole('button', { name: t.canvas.addSeat }));
    expect(store.dirty).toBe(true);
    await user.click(navLink(t.nav.settings));
    let dlg = screen.getByRole('alertdialog');
    await user.click(within(dlg).getByRole('button', { name: t.unsaved.cancel }));
    expect(store.prefs.tab).toBe('seats');
    await user.click(navLink(t.nav.settings));
    dlg = screen.getByRole('alertdialog');
    await user.click(within(dlg).getByRole('button', { name: t.unsaved.discard }));
    expect(store.prefs.tab).toBe('settings');
    expect(store.dirty).toBe(false);
    // No changes: no question.
    await user.click(navLink(t.nav.seats));
    expect(screen.queryByRole('alertdialog')).not.toBeInTheDocument();
    expect(store.prefs.tab).toBe('seats');
  });
});

describe('network tab', () => {
  it('assigns a static address with a rule and a program', async () => {
    const { api, store, user } = await setupNetwork();
    // The shared program list is edited on the page (Advanced > Network addresses).
    await user.click(screen.getByTestId('ip-program-add'));
    const path = screen.getByTestId('ip-program-path') as HTMLInputElement;
    await user.type(path, 'C:\\Oyunlar\\lan.exe');
    path.dispatchEvent(new Event('change', { bubbles: true }));
    const rows = screen.getAllByTestId('ip-row');
    await user.click(within(rows[1]).getByRole('button', { name: t.advancedPage.ipEditFor('Misafir') }));
    const dlg = await screen.findByRole('dialog', { name: t.ipDialog.title(2) });
    const select = within(dlg).getByLabelText(t.ipDialog.address) as HTMLSelectElement;
    // Only static addresses are offered (plus "not assigned").
    await vi.waitFor(() => expect(within(select).getAllByRole('option')).toHaveLength(3));
    expect(within(select).queryByText(/192\.168\.1\.57/)).not.toBeInTheDocument();
    await user.selectOptions(select, '192.168.1.21');
    await user.click(within(dlg).getByRole('radio', { name: new RegExp(`^${t.ipDialog.rules.only_listed}`) }));
    expect(within(dlg).getAllByText(t.ipDialog.listNote(t.network.programsCount(1))).length).toBeGreaterThan(0);
    await user.click(within(dlg).getByRole('button', { name: t.save }));
    expect(store.draft?.seats[1].ip_binding).toEqual({ address: '192.168.1.21', rule: 'only_listed' });
    expect(store.draft?.settings.ip_programs?.[0]).toEqual({ title: 'lan.exe', path: 'C:\\Oyunlar\\lan.exe', enabled: true });
    expect(screen.getAllByTestId('ip-row')[1]).toHaveTextContent('192.168.1.21');
    await store.save();
    expect(api.state.config.seats[1].ip_binding?.address).toBe('192.168.1.21');
  });

  it('says how the addresses are applied: driver or the environment fallback', async () => {
    const { store, user } = await setupNetwork();
    const box = await screen.findByTestId('ip-enforcement');
    await vi.waitFor(() => expect(box).toHaveTextContent(t.network.driverMissing(t.network.driverReasons.driver_not_loaded)));
    store.backends = {
      ...store.backends!,
      source_ip: { enforcement: 'driver', enforced: [2, 3], shared_session: [4], dropped_paths: ['Z:\\x.exe'] }
    };
    await vi.waitFor(() => expect(box).toHaveTextContent(t.network.enforcedByDriver('2., 3.')));
    expect(box).toHaveTextContent(t.network.driverScope);
    expect(box).toHaveTextContent(t.network.sharedSession('4.'));
    expect(box).toHaveTextContent(t.network.droppedPaths(1));
    store.backends = { ...store.backends!, source_ip: { enforcement: 'driver', enforced: [], shared_session: [], dropped_paths: [] } };
    await vi.waitFor(() => expect(box).toHaveTextContent(t.network.driverReady));
    // An older service sends no report: the stored-only hint.
    store.backends = { ...store.backends!, source_ip: undefined };
    await vi.waitFor(() => expect(box).toHaveTextContent(t.network.bestEffort));
  });

  it('adds programs from the browse and folder pickers without duplicates', async () => {
    const { store, user } = await setupNetwork();
    await user.click(screen.getByTestId('ip-browse'));
    await vi.waitFor(() => expect(store.draft?.settings.ip_programs?.length).toBe(1));
    await user.click(screen.getByTestId('ip-add-folder'));
    await vi.waitFor(() => expect(store.draft?.settings.ip_programs?.length).toBe(2));
    expect(store.draft?.settings.ip_programs?.map((p) => p.path)).toEqual(['C:\\Games\\game.exe', 'C:\\Games\\launcher.exe']);
  });
});

describe('dialogs', () => {
  it('outputs dialog: one output per monitor unless cloning is allowed', async () => {
    const onSave = vi.fn();
    const devices = demoInventory();
    render(OutputsDialog, { config: normalize(demoConfig()), devices, onSave, onCancel: vi.fn() });
    const user = userEvent.setup();
    const row = screen.getAllByTestId('outputs-row')[0];
    // Without cloning each monitor gets one output, chosen in a list.
    const pick = within(row).getByRole('combobox');
    const first = within(pick).getAllByRole('option')[1] as HTMLOptionElement;
    await user.selectOptions(pick, first.value);
    await user.click(screen.getByRole('switch', { name: t.outputs.forceAtStart }));
    await user.click(screen.getByRole('button', { name: t.save }));
    const r = onSave.mock.calls[0][0];
    expect(r.settings.force_assign_at_start).toBe(true);
    expect(r.monitors[0].outputs).toHaveLength(1);
  });

  it('picture dialog offers built-in and user pictures', async () => {
    const onSave = vi.fn();
    const kbd = demoInventory().find((d) => d.kind === 'keyboard')!;
    const loadUser = vi.fn(async () => [{ name: 'benim.png', data_url: 'data:image/png;base64,AA==' }]);
    render(PictureDialog, { device: kbd, loadUser, onSave, onCancel: vi.fn() });
    const user = userEvent.setup();
    await user.click(screen.getByRole('radio', { name: /Compact keyboard/ }));
    await user.click(await screen.findByRole('radio', { name: /benim\.png/ }));
    await user.click(screen.getByRole('button', { name: t.picture.choose }));
    expect(loadUser).toHaveBeenCalledWith('keyboard');
    expect(onSave).toHaveBeenCalledWith('user:benim.png');
  });
});

describe('wizard page', () => {
  it('shows the monitor number, then the keyboard step, and reports target hits', async () => {
    const api = createDemoApi();
    const mons = demoInventory().filter((d) => d.kind === 'monitor').map((d) => d.key);
    await api.wizardStart('all_seats');
    render(WizardPage, { api, monitor: mons[1] });
    expect(await screen.findByTestId('page-number')).toHaveTextContent('2');
    const user = userEvent.setup();
    await user.click(screen.getByRole('button', { name: t.wizard.pagePickButton }));
    // The demo service moves the picked monitor first: this page is seat 1 now.
    expect(await screen.findByRole('img', { name: t.wizard.pageSeat(1) })).toBeInTheDocument();
    const [before, after] = t.wizard.pageKeyHead();
    expect(screen.getByRole('heading', { level: 1 })).toHaveTextContent(`${before}F1${after}`);
    api.emit({
      type: 'wizard_progress',
      data: { step: 'mouse', seat: 1, key_presses: 0, clicks: 0, assigned: [[1, 'kbd;x']], monitors: [mons[1], mons[0]], confirmed: [] }
    });
    await user.click(await screen.findByRole('button', { name: t.wizard.pageTarget }));
    expect(api.calls.filter((c) => c === 'wizard_command')).toHaveLength(2);
  });
});

describe('wizard (main window)', () => {
  it('is not offered below the minimum resolution', async () => {
    localStorage.clear();
    const api = createDemoApi();
    for (const d of api.state.inventory) {
      if (d.details.type === 'display') d.details.mode = { width: 800, height: 600, refresh_mhz: 60000, rotation: 0 };
    }
    const store = new ManagerStore(api);
    await store.init();
    render(Wizard, { store, onClose: vi.fn() });
    expect(await screen.findByTestId('wizard-resolution')).toBeInTheDocument();
    expect(screen.getByRole('button', { name: t.wizard.start })).toHaveAttribute('aria-disabled', 'true');
  });

  it('finds the main devices by touch, starts, sends them and opens the pages', async () => {
    localStorage.clear();
    const api = createDemoApi();
    const store = new ManagerStore(api);
    await store.init();
    render(Wizard, { store, onClose: vi.fn() });
    const user = userEvent.setup();
    await user.click(screen.getByRole('button', { name: t.wizard.findByTouch }));
    const kbd = api.state.inventory.find((d) => d.kind === 'keyboard')!;
    const mouse = api.state.inventory.find((d) => d.kind === 'mouse')!;
    api.emit({ type: 'device_activity', data: { key: kbd.key } });
    api.emit({ type: 'device_activity', data: { key: mouse.key } });
    expect(await screen.findByText(t.wizard.gathered)).toBeInTheDocument();
    await user.click(screen.getByRole('button', { name: t.wizard.start }));
    await vi.waitFor(() => expect(api.calls).toContain('wizard_start'));
    expect(api.calls.filter((c) => c === 'wizard_command').length).toBeGreaterThanOrEqual(2);
  });

  it('walks from the monitor pick to the keyboard step with progress, found devices and the key legend', async () => {
    localStorage.clear();
    const api = createDemoApi();
    const store = new ManagerStore(api);
    await store.init();
    render(Wizard, { store, onClose: vi.fn() });
    const user = userEvent.setup();
    await user.click(screen.getByRole('radio', { name: new RegExp(t.wizard.variantOptions.all_seats.label) }));
    await user.click(screen.getByRole('button', { name: t.wizard.start }));
    expect(await screen.findByRole('heading', { level: 1, name: t.wizard.pickHead })).toBeInTheDocument();
    const mon = api.state.inventory.find((d) => d.kind === 'monitor' && d.present)!;
    await user.click(screen.getByRole('button', { name: t.wizard.pickThisAria(mon.name) }));
    const [before, after] = t.wizard.keyHead(1);
    expect(await screen.findByRole('heading', { level: 1, name: `${before}F1${after}` })).toBeInTheDocument();
    const steps = screen.getByRole('list', { name: t.wizard.progress });
    expect(steps.querySelector('[aria-current="step"]')).toHaveTextContent(t.wizard.pageSeat(1));
    expect(steps.querySelectorAll('li')).toHaveLength(api.state.inventory.filter((d) => d.kind === 'monitor' && d.present).length + 2);
    // The current seat's monitor is listed under the found devices.
    expect(screen.getByText(mon.name)).toBeInTheDocument();
    // Enter and Shift+Esc stay visible; the rest sits under "Other shortcuts".
    const legend = screen.getByRole('group', { name: t.wizard.keysIntro });
    expect(legend).toHaveTextContent(t.wizard.keysList.enter.does);
    expect(legend).toHaveTextContent(t.wizard.keysList.shiftEsc.does);
    expect(screen.getByText(t.wizard.moreKeys)).toBeInTheDocument();
    await user.click(screen.getByRole('button', { name: t.wizard.confirmSeat }));
    await vi.waitFor(() => expect(api.calls.filter((c) => c === 'wizard_command').length).toBeGreaterThanOrEqual(3));
  });
});

describe('enable dialog (redesign P8)', () => {
  it('shows the start mode and its Change button opens Settings', async () => {
    const { store, user } = await setup();
    await user.click(screen.getByRole('button', { name: t.power.openAndStart }));
    const dlg = screen.getByRole('alertdialog', { name: t.enable.enableTitle });
    expect(within(dlg).getByText(t.basic.startMode)).toBeInTheDocument();
    expect(within(dlg).getByText(t.settingsPage.startModeLabels[store.draft!.settings.start_mode])).toBeInTheDocument();
    await user.click(within(dlg).getByRole('button', { name: t.enable.startModeChangeLabel }));
    expect(screen.queryByRole('alertdialog')).not.toBeInTheDocument();
    expect(store.prefs.tab).toBe('settings');
  });
});

describe('help links (spec/local 10 §2.1)', () => {
  it('open the fixed help pages, and wait for a network', async () => {
    const { api, user } = await setup();
    await user.click(navLink(t.nav.settings));
    await user.click(await screen.findByRole('button', { name: new RegExp(t.settingsPage.manual) }));
    await user.click(screen.getByRole('button', { name: new RegExp(t.settingsPage.contact) }));
    expect(api.calls).toEqual(expect.arrayContaining(['open_help:manual', 'open_help:contact']));
    window.dispatchEvent(new Event('offline'));
    const site = await screen.findByRole('button', { name: new RegExp(t.settingsPage.website) });
    await vi.waitFor(() => expect(site).toBeDisabled());
    expect(site).toHaveTextContent(t.settingsPage.helpOffline);
    window.dispatchEvent(new Event('online'));
    await vi.waitFor(() => expect(site).toBeEnabled());
  });
});
