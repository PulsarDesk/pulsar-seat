// SPDX-License-Identifier: GPL-3.0-only
// The remote seats tab inside the one manager (merge step 9): create a remote seat, see its
// kind on the seats tab, edit it and keep the local draft.
import { render, screen, within } from '@testing-library/svelte';
import userEvent from '@testing-library/user-event';
import { describe, expect, it, vi } from 'vitest';
import App from '#lib/components/App.svelte';
import { createDemoApi } from '#lib/demo.ts';
import { rt, t } from '#lib/i18n.ts';
import { ManagerStore } from '#lib/store.svelte.ts';

async function setup(admin = true) {
  localStorage.clear();
  const api = createDemoApi({ admin });
  const store = new ManagerStore(api);
  await store.init();
  render(App, { store });
  const user = userEvent.setup();
  await screen.findByRole('link', { name: new RegExp(`^${t.nav.remote}`) });
  return { api, store, user };
}

describe('remote seats tab', () => {
  it('creates a remote seat through apply_config and lists it with its kind', async () => {
    const { api, user } = await setup();
    await user.click(screen.getByRole('link', { name: new RegExp(`^${t.nav.remote}`) }));
    expect(await screen.findByText(rt.nav.noSeats)).toBeInTheDocument();
    await user.click(screen.getByRole('button', { name: rt.nav.newSeat }));
    await user.type(screen.getByRole('textbox'), 'Salon');
    await user.click(screen.getByRole('button', { name: rt.newSeat.create }));
    const seat = api.state.config.seats.find((s) => s.name === 'Salon');
    expect(seat?.kind).toBe('remote');
    expect(api.calls).toContain('apply_config');
    // The seat page opens for the new seat.
    expect(await screen.findByRole('combobox', { name: rt.seat.streamBackend })).toBeInTheDocument();

    await user.click(screen.getByRole('link', { name: new RegExp(`^${t.nav.seats}`) }));
    const list = screen.getByRole('region', { name: t.seatKind.remoteList });
    // A link row: the name, "Seat N" as help and the state word.
    expect(within(list).getByText('Salon')).toBeInTheDocument();
    expect(within(list).getByText(t.seatN(seat!.number), { exact: false })).toBeInTheDocument();
    // No seat card (device container) for a remote seat.
    expect(screen.queryByRole('region', { name: 'Salon' })).toBeNull();

    await user.click(within(list).getByRole('button', { name: /Salon/ }));
    expect(await screen.findByRole('combobox', { name: rt.seat.streamBackend })).toBeInTheDocument();
    // First full App render of this file: ~1.7 s alone, over vitest's 5 s default when all
    // 46 test files run in parallel on the ARM64 dev box (2026-11-24).
  }, 15_000);

  it('docks the remote save bar in the shell and guards leaving with staged edits', async () => {
    const { api, user } = await setup();
    await user.click(screen.getByRole('link', { name: new RegExp(`^${t.nav.remote}`) }));
    await user.click(await screen.findByRole('button', { name: rt.nav.newSeat }));
    await user.type(screen.getByRole('textbox'), 'Salon');
    await user.click(screen.getByRole('button', { name: rt.newSeat.create }));
    const name = (await screen.findByRole('textbox', { name: rt.seat.displayName })) as HTMLInputElement;
    await user.type(name, '2');
    const typed = name.value;
    // One bar, docked by the shell under the scroll area (not inside the page).
    const bars = screen.getAllByRole('region', { name: t.saveBar.region });
    expect(bars).toHaveLength(1);
    expect(bars[0].closest('.page')).toBeNull();
    expect(bars[0].closest('main')).not.toBeNull();

    // Leaving asks first; Cancel keeps the draft.
    await user.click(screen.getByRole('link', { name: new RegExp(`^${t.nav.seats}`) }));
    const ask = await screen.findByRole('alertdialog', { name: t.unsaved.title });
    await user.click(within(ask).getByRole('button', { name: t.unsaved.cancel }));
    expect(name).toHaveValue(typed);

    const before = api.calls.filter((c) => c === 'apply_config').length;
    await user.click(within(bars[0]).getByRole('button', { name: t.saveBar.save }));
    await vi.waitFor(() => expect(api.calls.filter((c) => c === 'apply_config').length).toBeGreaterThan(before));
    const seat = api.state.config.seats.find((s) => s.name === 'Salon');
    expect(JSON.stringify(seat)).toContain(JSON.stringify(typed));
    expect(screen.queryByRole('region', { name: t.saveBar.region })).toBeNull();
  });

  it('a remote edit keeps unsaved local edits in the draft', async () => {
    const { api, store, user } = await setup();
    await user.click(screen.getByRole('button', { name: t.canvas.addSeat }));
    expect(store.dirty).toBe(true);
    await store.applyRemote((c) => ({ ...c, settings: { ...c.settings, remote: { ...c.settings.remote!, multi_session: true } } }));
    expect(api.state.config.settings.remote?.multi_session).toBe(true);
    expect(api.state.config.seats).toHaveLength(2);
    expect(store.draft?.seats).toHaveLength(3);
    expect(store.draft?.settings.remote?.multi_session).toBe(true);
  });

  it('standard users see the admin hint and send no remote request', async () => {
    const { api, user } = await setup(false);
    await user.click(screen.getByRole('link', { name: new RegExp(`^${t.nav.remote}`) }));
    expect(screen.getAllByText(t.adminNeeded).length).toBeGreaterThan(0);
    expect(api.calls.filter((c) => c.startsWith('get_host_info') || c === 'list_local_users')).toEqual([]);
  });
});
