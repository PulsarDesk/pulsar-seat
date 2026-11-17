// SPDX-License-Identifier: GPL-3.0-only
// Tab-leave guard, network tab + IP dialog, log level, outputs, pictures, wizard pages.
import { render, screen, within } from '@testing-library/svelte';
import userEvent from '@testing-library/user-event';
import { describe, expect, it, vi } from 'vitest';
import App from './App.svelte';
import LogLevelDialog from './LogLevelDialog.svelte';
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
  await user.click(await screen.findByRole('button', { name: t.later }));
  return { api, store, user };
}

describe('tab leave with unsaved changes', () => {
  it('asks save / discard / cancel and follows the answer', async () => {
    const { store, user } = await setup();
    await user.click(screen.getByRole('button', { name: t.canvas.addSeat }));
    expect(store.dirty).toBe(true);
    await user.click(screen.getByRole('tab', { name: t.tabs.basic }));
    let dlg = screen.getByRole('alertdialog');
    await user.click(within(dlg).getByRole('button', { name: t.unsaved.cancel }));
    expect(store.prefs.tab).toBe('seats');
    await user.click(screen.getByRole('tab', { name: t.tabs.basic }));
    dlg = screen.getByRole('alertdialog');
    await user.click(within(dlg).getByRole('button', { name: t.unsaved.discard }));
    expect(store.prefs.tab).toBe('basic');
    expect(store.dirty).toBe(false);
    // No changes: no question.
    await user.click(screen.getByRole('tab', { name: t.tabs.seats }));
    expect(screen.queryByRole('alertdialog')).not.toBeInTheDocument();
    expect(store.prefs.tab).toBe('seats');
  });
});

describe('network tab', () => {
  it('assigns a static address with a rule and a program', async () => {
    const { api, store, user } = await setup();
    await user.click(screen.getByRole('tab', { name: t.tabs.network }));
    const rows = screen.getAllByTestId('ip-row');
    await user.click(within(rows[1]).getByRole('button', { name: t.network.edit }));
    const dlg = await screen.findByRole('dialog', { name: t.ipDialog.title(2) });
    const select = within(dlg).getByTestId('ip-address') as HTMLSelectElement;
    // Only static addresses are offered.
    await vi.waitFor(() => expect(within(select).getAllByRole('option')).toHaveLength(3));
    expect(within(select).queryByText(/192\.168\.1\.57/)).not.toBeInTheDocument();
    await user.selectOptions(select, '192.168.1.21');
    await user.click(within(dlg).getByLabelText(t.ipDialog.rules.only_listed));
    await user.type(within(dlg).getByTestId('ip-new-path'), 'C:\\Oyunlar\\lan.exe');
    await user.click(within(dlg).getByRole('button', { name: t.ipDialog.add }));
    await user.click(within(dlg).getByRole('button', { name: t.save }));
    expect(store.draft?.seats[1].ip_binding).toEqual({ address: '192.168.1.21', rule: 'only_listed' });
    expect(store.draft?.settings.ip_programs?.[0]).toEqual({ title: 'lan.exe', path: 'C:\\Oyunlar\\lan.exe', enabled: true });
    expect(screen.getAllByTestId('ip-row')[1]).toHaveTextContent('192.168.1.21');
    await user.click(screen.getByRole('button', { name: t.bar.save }));
    expect(api.state.config.seats[1].ip_binding?.address).toBe('192.168.1.21');
  });

  it('says how the addresses are applied: driver or the environment fallback', async () => {
    const { store, user } = await setup();
    await user.click(screen.getByRole('tab', { name: t.tabs.network }));
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
    const { store, user } = await setup();
    await user.click(screen.getByRole('tab', { name: t.tabs.network }));
    await user.click(within(screen.getAllByTestId('ip-row')[1]).getByRole('button', { name: t.network.edit }));
    const dlg = await screen.findByRole('dialog', { name: t.ipDialog.title(2) });
    await user.click(within(dlg).getByTestId('ip-browse'));
    await user.click(within(dlg).getByTestId('ip-add-folder'));
    await user.click(within(dlg).getByRole('button', { name: t.save }));
    expect(store.draft?.settings.ip_programs?.map((p) => p.path)).toEqual(['C:\\Games\\game.exe', 'C:\\Games\\launcher.exe']);
  });
});

describe('dialogs', () => {
  it('log level dialog returns the switches', async () => {
    const onSave = vi.fn();
    render(LogLevelDialog, { levels: { info: true, debug: false, trace: false }, onSave, onCancel: vi.fn() });
    const user = userEvent.setup();
    await user.click(screen.getByLabelText(t.logLevel.debug));
    await user.click(screen.getByRole('button', { name: t.save }));
    expect(onSave).toHaveBeenCalledWith({ info: true, debug: true, trace: false });
  });

  it('outputs dialog: one output per monitor unless cloning is allowed', async () => {
    const onSave = vi.fn();
    const devices = demoInventory();
    render(OutputsDialog, { config: normalize(demoConfig()), devices, onSave, onCancel: vi.fn() });
    const user = userEvent.setup();
    const row = screen.getAllByTestId('outputs-row')[0];
    const boxes = within(row).getAllByRole('checkbox');
    await user.click(boxes[0]);
    if (boxes.length > 2) {
      await user.click(boxes[1]);
      // Without cloning the second pick replaces the first.
      expect((boxes[0] as HTMLInputElement).checked).toBe(false);
    }
    await user.click(screen.getByLabelText(t.outputs.forceAtStart));
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
    expect(await screen.findByText(t.wizard.pageSeat(1))).toBeInTheDocument();
    expect(screen.getByText(t.wizard.keyboard(1))).toBeInTheDocument();
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
    expect(screen.getByRole('button', { name: t.wizard.start })).toBeDisabled();
  });

  it('finds the main devices by touch, starts, sends them and opens the pages', async () => {
    localStorage.clear();
    const api = createDemoApi();
    const store = new ManagerStore(api);
    await store.init();
    render(Wizard, { store, onClose: vi.fn() });
    const user = userEvent.setup();
    await user.click(screen.getByRole('button', { name: t.wizard.gather }));
    const kbd = api.state.inventory.find((d) => d.kind === 'keyboard')!;
    const mouse = api.state.inventory.find((d) => d.kind === 'mouse')!;
    api.emit({ type: 'device_activity', data: { key: kbd.key } });
    api.emit({ type: 'device_activity', data: { key: mouse.key } });
    expect(await screen.findByText(t.wizard.gathered)).toBeInTheDocument();
    await user.click(screen.getByRole('button', { name: t.wizard.start }));
    await vi.waitFor(() => expect(api.calls).toContain('wizard_start'));
    expect(api.calls.filter((c) => c === 'wizard_command').length).toBeGreaterThanOrEqual(2);
  });
});
