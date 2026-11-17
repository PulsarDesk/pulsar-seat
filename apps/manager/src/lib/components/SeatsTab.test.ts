// SPDX-License-Identifier: GPL-3.0-only
// Seats board (redesign P3): Move menu without dragging, live announcements, seat menu,
// status line, restart required, missing-device lines, drag and drop.
import { fireEvent, render, screen, within } from '@testing-library/svelte';
import userEvent from '@testing-library/user-event';
import { tick } from 'svelte';
import { describe, expect, it, vi } from 'vitest';
import SeatsTab, { type SeatsTabCommands } from './SeatsTab.svelte';
import { createDemoApi } from '#lib/demo.ts';
import { t } from '#lib/i18n.ts';
import { removeSeat } from '#lib/model.ts';
import { ManagerStore } from '#lib/store.svelte.ts';
import type { SeatNumber } from '#lib/types.ts';

const KBD = 'kbd;loc=USB(2-1-1)';

async function setup(extra: { restartRequired?: SeatNumber[]; commands?: Partial<SeatsTabCommands> } = {}) {
  localStorage.clear();
  const api = createDemoApi();
  const store = new ManagerStore(api);
  await store.init();
  const commands: SeatsTabCommands = {
    startSeat: vi.fn(),
    wizard: vi.fn(),
    cpu: vi.fn(),
    moveAssignment: vi.fn(),
    ipBinding: vi.fn(),
    openRemote: vi.fn(),
    enable: vi.fn(),
    startAll: vi.fn(),
    restartSeat: vi.fn(),
    ...extra.commands
  };
  render(SeatsTab, { store, commands, restartRequired: extra.restartRequired });
  const user = userEvent.setup();
  return { api, store, commands, user };
}

function maskOf(store: ManagerStore, key: string) {
  return store.draft?.devices.find((d) => d.key === key)?.mask;
}

describe('Seats board: move without dragging', () => {
  it('Enter on a device row opens the Move menu; a seat moves it and the live region says so', async () => {
    const { store, user } = await setup();
    const two = screen.getByRole('region', { name: 'Misafir' });
    const row = within(two).getByRole('button', { name: t.device.actions('Kablosuz klavye') });
    row.focus();
    await user.keyboard('{Enter}');
    const menu = await screen.findByRole('menu', { name: t.device.actions('Kablosuz klavye') });
    expect(row).toHaveAttribute('aria-expanded', 'true');
    // Text only: no icon inside a menu item.
    for (const mi of within(menu).getAllByRole('menuitem')) expect(mi.querySelector('svg')).toBeNull();
    // The seat it is on cannot be chosen again; seat 1 can.
    expect(within(menu).getByRole('menuitem', { name: /Misafir/ })).toHaveAttribute('aria-disabled', 'true');
    await user.click(within(menu).getByRole('menuitem', { name: t.seatN(1) }));
    expect(maskOf(store, KBD)).toBe(1);
    expect(screen.getByTestId('move-announce')).toHaveTextContent(t.move.announce('Kablosuz klavye', t.seatN(1)));
    // The row now lives in seat 1 and says it is not saved yet.
    const one = screen.getByRole('region', { name: t.seatN(1) });
    expect(within(one).getByText(t.device.moved)).toBeInTheDocument();
    expect(screen.queryByRole('menu')).toBeNull();
  });

  it('moves to the pool, shares a shareable device and opens the details', async () => {
    const { store, user } = await setup();
    await user.click(screen.getByRole('button', { name: t.device.actions('Kablosuz klavye') }));
    await user.click(screen.getByRole('menuitem', { name: t.move.toPool }));
    expect(maskOf(store, KBD)).toBe(0);
    expect(screen.getByTestId('move-announce')).toHaveTextContent(t.move.announcePool('Kablosuz klavye'));
    expect(within(screen.getByRole('region', { name: t.canvas.system })).getByText('Kablosuz klavye')).toBeInTheDocument();

    // A keyboard is exclusive: no "share" item. A headset is shareable.
    await user.click(screen.getByRole('button', { name: t.device.actions('Kablosuz klavye') }));
    expect(screen.queryByRole('menuitem', { name: t.move.share })).toBeNull();
    await user.keyboard('{Escape}');
    await user.click(screen.getByRole('button', { name: t.device.actions('USB headset') }));
    await user.click(screen.getByRole('menuitem', { name: t.move.share }));
    expect(screen.getByTestId('move-announce')).toHaveTextContent(t.move.announceShare('USB headset'));

    await user.click(screen.getByRole('button', { name: t.device.actions('Kablosuz klavye') }));
    await user.click(screen.getByRole('menuitem', { name: t.device.infoMenu }));
    expect(await screen.findByRole('dialog', { name: 'Kablosuz klavye' })).toBeInTheDocument();
  });

  it('a monitor face opens the same menu and a drop moves a device', async () => {
    const { store, user } = await setup();
    const face = screen.getByRole('button', { name: t.device.faceActions('Harici ekran B', '2.1') });
    await user.click(face);
    expect(await screen.findByRole('menuitem', { name: t.deviceMenu.outputs })).toBeInTheDocument();
    await user.keyboard('{Escape}');
    expect(face).toHaveFocus();

    const one = screen.getByRole('region', { name: t.seatN(1) });
    const payload = JSON.stringify({ key: KBD, from: 2 });
    await fireEvent.dragEnter(one, { dataTransfer: { getData: () => payload } });
    expect(one.className).toContain('drop');
    await fireEvent.drop(one, { dataTransfer: { getData: () => payload } });
    expect(maskOf(store, KBD)).toBe(1);
    expect(one.className).not.toContain('drop');
  });
});

describe('Seats board: cards and head', () => {
  it('the seat menu holds the run actions and every seat dialog', async () => {
    const { store, commands, user } = await setup();
    const two = screen.getByRole('region', { name: 'Misafir' });
    await user.click(within(two).getByRole('button', { name: t.seatCard.actions('Misafir') }));
    const menu = screen.getByRole('menu', { name: t.seatCard.actions('Misafir') });
    for (const label of [t.seatMenu.start, t.seatMenu.rename, t.seatMenu.account, t.seatCard.ipMenu, t.seatMenu.cpu, t.seatCard.gpuMenu, t.seatMenu.compat]) {
      expect(within(menu).getByRole('menuitem', { name: label })).toBeInTheDocument();
    }
    await user.click(within(menu).getByRole('menuitem', { name: t.settingsPage.padsOn }));
    expect(store.draft?.seats[1].local?.virtual_pads).toBe(true);

    await user.click(within(two).getByRole('button', { name: t.seatCard.actions('Misafir') }));
    await user.click(screen.getByRole('menuitem', { name: t.seatCard.ipMenu }));
    expect(commands.ipBinding).toHaveBeenCalledWith(2);

    await user.click(within(two).getByRole('button', { name: t.seatCard.actions('Misafir') }));
    await user.click(screen.getByRole('menuitem', { name: t.seatCard.gpuMenu }));
    expect(await screen.findByRole('dialog')).toBeInTheDocument();
  });

  it('delete asks first, with the seat name', async () => {
    const { store, user } = await setup();
    const two = screen.getByRole('region', { name: 'Misafir' });
    await user.click(within(two).getByRole('button', { name: t.seatCard.actions('Misafir') }));
    await user.click(screen.getByRole('menuitem', { name: t.seatCard.removeMenu }));
    const dlg = screen.getByRole('alertdialog', { name: t.remove.titleNamed('Misafir') });
    await user.click(within(dlg).getByRole('button', { name: t.seatMenu.remove }));
    expect(store.draft?.seats.map((s) => s.number)).toEqual([1]);
  });

  it('kiosk seat is offered only with the desktops session method', async () => {
    const { store, user } = await setup();
    const open = async () => {
      await user.click(within(screen.getByRole('region', { name: 'Misafir' })).getByRole('button', { name: t.seatCard.actions('Misafir') }));
      return screen.getByRole('menu', { name: t.seatCard.actions('Misafir') });
    };
    expect(within(await open()).queryByRole('menuitem', { name: t.seatCard.kioskMenu })).toBeNull();
    await user.keyboard('{Escape}');
    store.edit((c) => ({ ...c, settings: { ...c.settings, backends: { ...c.settings.backends, session: 'desktops' } } }));
    await tick();
    expect(within(await open()).getByRole('menuitem', { name: t.seatCard.kioskMenu })).toBeInTheDocument();
  });

  it('off: the status line offers "Turn on and start" and Add seat drops to secondary', async () => {
    const { commands, user } = await setup();
    expect(screen.getByText(t.power.off)).toBeInTheDocument();
    await user.click(screen.getByRole('button', { name: t.power.openAndStart }));
    expect(commands.enable).toHaveBeenCalledWith(true);
    expect(screen.getByRole('button', { name: t.canvas.addSeat })).toHaveClass('secondary');
  });

  it('restart required shows on the card with its own button', async () => {
    const { commands, user } = await setup({ restartRequired: [2] });
    const two = screen.getByRole('region', { name: 'Misafir' });
    expect(within(two).getByText(t.seatCard.restartNeeded)).toBeInTheDocument();
    await user.click(within(two).getByRole('button', { name: t.seatCard.restart }));
    expect(commands.restartSeat).toHaveBeenCalledWith(2);
    expect(within(screen.getByRole('region', { name: t.seatN(1) })).queryByText(t.seatCard.restartNeeded)).toBeNull();
  });

  it('an empty new seat lists what it lacks; screen and keyboard block', async () => {
    const { user } = await setup();
    await user.click(screen.getByRole('button', { name: t.canvas.addSeat }));
    const three = screen.getByRole('region', { name: t.seatN(3) });
    expect(three.querySelector('[data-missing="keyboard"]')).toHaveClass('block');
    expect(three.querySelector('[data-missing="display"]')).toHaveClass('block');
    expect(three.querySelector('[data-missing="mouse"]')).not.toHaveClass('block');
    // The demo speakers are shared by every seat: nothing to report for sound.
    expect(three.querySelector('[data-missing="audio"]')).toBeNull();
    expect(within(three).getByText(t.slot.keyboard, { exact: false })).toBeInTheDocument();
  });

  it('one local seat: the "add a second seat" invitation starts the wizard', async () => {
    const { store, commands, user } = await setup();
    store.edit((c) => removeSeat(c, 2));
    await tick();
    await user.click(await screen.findByRole('button', { name: t.canvas.startWizard }));
    expect(commands.wizard).toHaveBeenCalled();
  });

  it('the overflow menu has the find, view and Windows settings groups', async () => {
    const { user } = await setup();
    await user.click(screen.getByRole('button', { name: t.canvas.more }));
    const menu = screen.getByRole('menu', { name: t.canvas.more });
    for (const label of [t.canvas.pickByTouch, t.canvas.identifyMonitors, t.systemMenu.wizard, t.systemMenu.view, t.systemMenu.compare, t.power.stopAll, t.canvas.windowsSettings]) {
      expect(within(menu).getByRole('menuitem', { name: label })).toBeInTheDocument();
    }
    await user.click(within(menu).getByRole('menuitem', { name: t.systemMenu.view }));
    const dlg = await screen.findByRole('dialog', { name: t.view.title });
    expect(within(dlg).queryByText(t.view.evenRows)).toBeNull();
  });
});
